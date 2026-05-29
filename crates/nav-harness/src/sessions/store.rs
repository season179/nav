//! SQLite-backed session store facade used by the server and tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use nav_types::{MessageId, RunId, SessionId, ToolCallId};
use serde_json::Value;

use super::canonical::{
    ModelTurn, ModelTurnRole, Part, ToolCall, Turn, TurnMeta, TurnPart, TurnRole,
};
use super::sqlite::{
    CreateSession, RunStatus, SqliteSessionStore, SqliteStoreError, StartRun, StoredPart,
    StoredTurn,
};

#[derive(Debug)]
pub struct SessionStore {
    sqlite: SqliteSessionStore,
}

impl SessionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        Ok(Self {
            sqlite: SqliteSessionStore::open(path)?,
        })
    }

    pub fn create_session(&self, session_id: SessionId) -> Result<(), SqliteStoreError> {
        self.create_session_with_record(session_id, default_session_record())
    }

    pub fn create_session_with_record(
        &self,
        session_id: SessionId,
        session: CreateSession,
    ) -> Result<(), SqliteStoreError> {
        self.sqlite.create_session(session_id, session)
    }

    pub fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<nav_types::SessionRow, SqliteStoreError> {
        self.sqlite.get_session(session_id)
    }

    pub fn start_run(&self, session_id: &SessionId, run_id: RunId) -> Result<(), SqliteStoreError> {
        self.sqlite.start_run(StartRun {
            id: run_id,
            session_id: session_id.clone(),
            status: RunStatus::Running,
            trigger: Some("session.sendMessage".to_string()),
            started_at: unix_millis(),
        })
    }

    pub fn finish_run(&self, run_id: &RunId, status: RunStatus) -> Result<(), SqliteStoreError> {
        self.sqlite.finish_run(run_id, status, unix_millis(), None)
    }

    pub fn append_turn(
        &self,
        run_id: &RunId,
        message_id: MessageId,
        turn: ModelTurn,
    ) -> Result<(), SqliteStoreError> {
        self.append_turns_with_first_id(run_id, vec![turn], Some(message_id))
    }

    pub fn append_turns(
        &self,
        run_id: &RunId,
        turns: Vec<ModelTurn>,
    ) -> Result<(), SqliteStoreError> {
        self.append_turns_with_first_id(run_id, turns, None)
    }

    pub fn turns(&self, session_id: &SessionId) -> Vec<ModelTurn> {
        self.try_turns(session_id).unwrap_or_default()
    }

    pub fn try_turns(&self, session_id: &SessionId) -> Result<Vec<ModelTurn>, SqliteStoreError> {
        let mut page = self
            .sqlite
            .list_turns_for_session(session_id, None, usize::MAX)?
            .items;
        page.reverse();
        Ok(page
            .into_iter()
            .filter_map(model_turn_from_stored_turn)
            .collect())
    }

    pub fn try_turns_for_run(&self, run_id: &RunId) -> Result<Vec<ModelTurn>, SqliteStoreError> {
        Ok(self
            .sqlite
            .list_turns_for_run(run_id)?
            .into_iter()
            .filter_map(model_turn_from_stored_turn)
            .collect())
    }

    fn append_turns_with_first_id(
        &self,
        run_id: &RunId,
        turns: Vec<ModelTurn>,
        first_message_id: Option<MessageId>,
    ) -> Result<(), SqliteStoreError> {
        let mut tool_call_ids = HashMap::new();
        let mut first_message_id = first_message_id;
        let mut stored_turns = Vec::new();

        for model_turn in turns {
            let Some(role) = stored_role(model_turn.role) else {
                continue;
            };
            let message_id = first_message_id.take().unwrap_or_else(new_message_id);
            let created_at = unix_millis();
            let parts = model_parts(&model_turn.parts, &mut tool_call_ids);
            stored_turns.push((
                Turn {
                    id: message_id,
                    run_id: run_id.clone(),
                    seq: 0,
                    role,
                    meta: TurnMeta::default(),
                    created_at,
                },
                parts,
            ));
        }

        self.sqlite.append_turns(&stored_turns)
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::open(ephemeral_db_path()).expect("ephemeral session store should open")
    }
}

fn default_session_record() -> CreateSession {
    CreateSession {
        title: None,
        source: "api".to_string(),
        workspace_root: None,
        system_prompt: None,
        settings_json: "{}".to_string(),
        parent_id: None,
        version: env!("CARGO_PKG_VERSION").to_string(),
        slug: None,
        created_at: unix_millis(),
    }
}

fn stored_role(role: ModelTurnRole) -> Option<TurnRole> {
    match role {
        ModelTurnRole::System => None,
        ModelTurnRole::User => Some(TurnRole::User),
        ModelTurnRole::Assistant | ModelTurnRole::Tool => Some(TurnRole::Assistant),
    }
}

fn model_parts(parts: &[TurnPart], tool_call_ids: &mut HashMap<String, ToolCallId>) -> Vec<Part> {
    parts
        .iter()
        .map(|part| model_part(part, tool_call_ids))
        .collect()
}

fn model_part(part: &TurnPart, tool_call_ids: &mut HashMap<String, ToolCallId>) -> Part {
    match part {
        TurnPart::Text(text) => Part::Text {
            text: text.clone(),
            synthetic: None,
        },
        TurnPart::ToolCall(tool_call) => {
            let id = tool_call
                .tool_call_id
                .clone()
                .unwrap_or_else(new_tool_call_id);
            tool_call_ids.insert(tool_call.id.clone(), id.clone());
            Part::ToolCall {
                id,
                name: tool_call.name.clone(),
                arguments: tool_call_arguments(&tool_call.arguments),
                raw_arguments_artifact_id: None,
            }
        }
        TurnPart::ToolResult {
            tool_call_id,
            content,
        } => {
            let call_id = stored_tool_call_id(tool_call_id, tool_call_ids);
            Part::ToolResult {
                call_id,
                content: content.clone(),
                raw_artifact_id: None,
                is_error: false,
            }
        }
    }
}

fn stored_tool_call_id(
    provider_tool_call_id: &str,
    tool_call_ids: &HashMap<String, ToolCallId>,
) -> ToolCallId {
    tool_call_ids
        .get(provider_tool_call_id)
        .cloned()
        .or_else(|| ToolCallId::try_new(provider_tool_call_id.to_string()).ok())
        .unwrap_or_else(new_tool_call_id)
}

fn tool_call_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_string()))
}

fn model_turn_from_stored_turn((turn, parts): StoredTurn) -> Option<ModelTurn> {
    let model_parts = parts
        .into_iter()
        .filter(|part| part.compacted_at.is_none())
        .filter_map(model_part_from_stored_part)
        .collect::<Vec<_>>();

    if model_parts.is_empty() {
        return None;
    }

    let role = if model_parts
        .iter()
        .all(|part| matches!(part, TurnPart::ToolResult { .. }))
    {
        ModelTurnRole::Tool
    } else {
        match turn.role {
            TurnRole::User => ModelTurnRole::User,
            TurnRole::Assistant => ModelTurnRole::Assistant,
        }
    };

    Some(ModelTurn {
        role,
        parts: model_parts,
    })
}

fn model_part_from_stored_part(part: StoredPart) -> Option<TurnPart> {
    match part.part {
        Part::Text { text, .. } => Some(TurnPart::Text(text)),
        Part::ToolCall {
            id,
            name,
            arguments,
            ..
        } => Some(TurnPart::ToolCall(ToolCall {
            id: id.to_string(),
            tool_call_id: Some(id),
            name,
            arguments: stored_tool_call_arguments(arguments),
        })),
        Part::ToolResult {
            call_id, content, ..
        } => Some(TurnPart::ToolResult {
            tool_call_id: call_id.to_string(),
            content,
        }),
        _ => None,
    }
}

fn ephemeral_db_path() -> PathBuf {
    static NEXT_DB: AtomicU64 = AtomicU64::new(0);

    let counter = NEXT_DB.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nav-session-{}-{}-{counter}.db",
        std::process::id(),
        unix_millis()
    ))
}

fn new_message_id() -> MessageId {
    MessageId::try_new(new_uuid_v7_string()).expect("generated message id should be UUIDv7")
}

fn new_tool_call_id() -> ToolCallId {
    ToolCallId::try_new(new_uuid_v7_string()).expect("generated tool call id should be UUIDv7")
}

fn new_uuid_v7_string() -> String {
    static NEXT_UUID: AtomicU64 = AtomicU64::new(0);

    let timestamp = unix_millis() as u64 & 0xffff_ffff_ffff;
    let sequence = NEXT_UUID.fetch_add(1, Ordering::Relaxed)
        ^ u64::from(std::process::id())
        ^ timestamp.rotate_left(13);

    format!(
        "{:08x}-{:04x}-7{:03x}-{:04x}-{:012x}",
        (timestamp >> 16) as u32,
        (timestamp & 0xffff) as u16,
        ((sequence >> 62) & 0x0fff) as u16,
        0x8000 | (((sequence >> 48) & 0x3fff) as u16),
        sequence & 0xffff_ffff_ffff
    )
}

fn unix_millis() -> i64 {
    static LAST_MILLIS: AtomicU64 = AtomicU64::new(0);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);

    let mut previous = LAST_MILLIS.load(Ordering::Relaxed);
    loop {
        let next = if now > previous { now } else { previous + 1 };
        match LAST_MILLIS.compare_exchange(previous, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next as i64,
            Err(observed) => previous = observed,
        }
    }
}

fn stored_tool_call_arguments(arguments: Value) -> String {
    match arguments {
        Value::String(raw) => raw,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_preserves_raw_invalid_tool_call_arguments() {
        let store = SessionStore::default();
        let session_id = session_id();
        let run_id = run_id();
        let tool_call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();

        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turns(
                &run_id,
                vec![ModelTurn::assistant_tool_calls(vec![ToolCall {
                    id: "provider-call-1".to_string(),
                    tool_call_id: Some(tool_call_id),
                    name: "read".to_string(),
                    arguments: "{invalid json".to_string(),
                }])],
            )
            .unwrap();

        let reloaded = store.try_turns(&session_id).unwrap();

        assert_eq!(
            reloaded[0].tool_calls()[0].arguments,
            "{invalid json".to_string()
        );
    }

    fn session_id() -> SessionId {
        SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap()
    }

    fn run_id() -> RunId {
        RunId::try_new("019f2f6f-f178-7a72-9f28-000000000002").unwrap()
    }
}
