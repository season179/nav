//! SQLite-backed session store facade used by the server and tests.

use std::collections::HashMap;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use nav_types::{MessageId, ProviderPayloadId, ProviderPayloadRow, RunId, SessionId, ToolCallId};
use serde_json::{Value, json};

use crate::models::{
    ChatGptSubscriptionDecodeInput, ChatGptSubscriptionDecoder, DecodedProviderPayload, Decoder,
    OpenAiChatCompletionsDecodeInput, OpenAiChatCompletionsDecoder, OpenAiResponsesDecodeInput,
    OpenAiResponsesDecoder,
};

use super::canonical::{
    ModelTurn, ModelTurnRole, Part, ToolCall, Turn, TurnMeta, TurnPart, TurnRole,
};
use super::sqlite::{
    CreateSession, NewProviderPayload, ProviderState, RunStatus, SqliteSessionStore,
    SqliteStoreError, StartRun, StoredPart, StoredTurn,
};

pub(crate) const OPENAI_CHAT_COMPLETIONS_DECODER_VERSION: &str =
    "openai-chat-completions-decoder@1";
pub(crate) const CHATGPT_SUBSCRIPTION_DECODER_VERSION: &str = "chatgpt-subscription-decoder@1";
pub(crate) const OPENAI_RESPONSES_DECODER_VERSION: &str = "openai-responses-decoder@1";
const UNKNOWN_DECODER_VERSION: &str = "unknown-decoder";

const DEFAULT_PAYLOAD_DECODERS: &[PayloadDecoder] = &[
    PayloadDecoder {
        api_kinds: &["openai_chat_completions", "openai-completions"],
        version: OPENAI_CHAT_COMPLETIONS_DECODER_VERSION,
        decode: decode_openai_chat_completions_payload,
    },
    PayloadDecoder {
        api_kinds: &[
            "chatgpt_subscription",
            "chatgpt-subscription",
            "codex_subscription",
            "codex-subscription",
        ],
        version: CHATGPT_SUBSCRIPTION_DECODER_VERSION,
        decode: decode_chatgpt_subscription_payload,
    },
    PayloadDecoder {
        api_kinds: &["openai_responses", "openai-responses"],
        version: OPENAI_RESPONSES_DECODER_VERSION,
        decode: decode_openai_responses_payload,
    },
];

#[derive(Clone, Copy)]
struct PayloadDecoder {
    api_kinds: &'static [&'static str],
    version: &'static str,
    decode: fn(&ProviderPayloadRow, Vec<u8>) -> Result<DecodedProviderPayload, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadRecoveryReport {
    pub diffs: Vec<ProviderPayloadDiff>,
}

impl PayloadRecoveryReport {
    pub fn to_text(&self) -> String {
        let mut output = format!("provider payload diffs: {}\n", self.diffs.len());
        for diff in &self.diffs {
            output.push_str(&format!(
                "payload {} {}: {} -> {}\n",
                diff.payload_id,
                diff.api_kind,
                diff.stored_decoder_version.as_deref().unwrap_or("<none>"),
                diff.current_decoder_version
            ));
            output.push_str(&format!("  stored: {}\n", diff.stored_parts_json));
            output.push_str(&format!("  decoded: {}\n", diff.decoded_parts_json));
        }
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderPayloadDiff {
    pub payload_id: ProviderPayloadId,
    pub api_kind: String,
    pub stored_decoder_version: Option<String>,
    pub current_decoder_version: String,
    pub stored_parts_json: String,
    pub decoded_parts_json: String,
}

#[derive(Debug)]
pub struct SessionStore {
    sqlite: SqliteSessionStore,
}

impl SessionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        let store = Self {
            sqlite: SqliteSessionStore::open(path)?,
        };
        store.recover_pending_provider_payloads()?;
        Ok(store)
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

    pub fn provider_payload_recovery_report(
        &self,
    ) -> Result<PayloadRecoveryReport, SqliteStoreError> {
        self.provider_payload_recovery_report_with_decoders(DEFAULT_PAYLOAD_DECODERS)
    }

    pub fn append_provider_payload(
        &self,
        payload: NewProviderPayload,
    ) -> Result<ProviderPayloadId, SqliteStoreError> {
        self.sqlite.append_provider_payload(payload)
    }

    pub fn get_provider_payload(
        &self,
        id: &ProviderPayloadId,
    ) -> Result<ProviderPayloadRow, SqliteStoreError> {
        self.sqlite.get_provider_payload(id)
    }

    pub fn append_decoded_provider_payload(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        decoded: &DecodedProviderPayload,
    ) -> Result<(), SqliteStoreError> {
        self.sqlite
            .append_decoded_provider_payload(id, decoder_version, decoded)
    }

    pub fn append_decoded_provider_payload_with_provider_state(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        decoded: &DecodedProviderPayload,
        provider_state: Option<&ProviderState>,
    ) -> Result<(), SqliteStoreError> {
        self.sqlite
            .append_decoded_provider_payload_with_provider_state(
                id,
                decoder_version,
                decoded,
                provider_state,
            )
    }

    pub fn get_provider_state(
        &self,
        run_id: &RunId,
    ) -> Result<Option<ProviderState>, SqliteStoreError> {
        self.sqlite.get_provider_state(run_id)
    }

    pub fn set_provider_state(&self, state: ProviderState) -> Result<(), SqliteStoreError> {
        self.sqlite.set_provider_state(state)
    }

    pub fn next_turn_created_at_for_run(
        &self,
        run_id: &RunId,
        now: i64,
    ) -> Result<i64, SqliteStoreError> {
        self.sqlite.next_turn_created_at_for_run(run_id, now)
    }

    fn append_turns_with_first_id(
        &self,
        run_id: &RunId,
        turns: Vec<ModelTurn>,
        first_message_id: Option<MessageId>,
    ) -> Result<(), SqliteStoreError> {
        let mut tool_call_ids = HashMap::new();
        let mut first_message_id = first_message_id;
        let mut created_at = self.next_turn_created_at_for_run(run_id, unix_millis())?;
        let mut stored_turns = Vec::new();

        for model_turn in turns {
            let Some(role) = stored_role(model_turn.role) else {
                continue;
            };
            let message_id = first_message_id.take().unwrap_or_else(new_message_id);
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
            created_at = created_at.saturating_add(1);
        }

        self.sqlite.append_turns(&stored_turns)
    }

    fn recover_pending_provider_payloads(&self) -> Result<usize, SqliteStoreError> {
        self.recover_pending_provider_payloads_with_decoders(DEFAULT_PAYLOAD_DECODERS)
    }

    fn recover_pending_provider_payloads_with_decoders(
        &self,
        decoders: &[PayloadDecoder],
    ) -> Result<usize, SqliteStoreError> {
        let pending_payloads = self.sqlite.list_pending_provider_payloads()?;
        let recovered = pending_payloads.len();

        for payload in pending_payloads {
            self.recover_pending_provider_payload(&payload, decoders)?;
        }

        Ok(recovered)
    }

    fn recover_pending_provider_payload(
        &self,
        payload: &ProviderPayloadRow,
        decoders: &[PayloadDecoder],
    ) -> Result<(), SqliteStoreError> {
        if payload.direction == "request" {
            return self.mark_provider_payload_ignored(
                &payload.id,
                "request_payload",
                "request envelopes are journaled for audit and replay, not decoded".to_string(),
            );
        }

        let Some(decoder) = decoder_for_api_kind(decoders, payload.api_kind.as_str()) else {
            return self.mark_provider_payload_failed(
                &payload.id,
                UNKNOWN_DECODER_VERSION,
                "unknown_api_kind",
                format!("no decoder registered for api_kind `{}`", payload.api_kind),
            );
        };

        let raw_bytes = match self.read_provider_payload_bytes(payload) {
            Ok(raw_bytes) => raw_bytes,
            Err(error) => {
                return self.mark_provider_payload_failed(
                    &payload.id,
                    decoder.version,
                    "artifact_read_failed",
                    error,
                );
            }
        };

        let decoded = match catch_unwind(AssertUnwindSafe(|| (decoder.decode)(payload, raw_bytes)))
        {
            Ok(Ok(decoded)) => decoded,
            Ok(Err(error)) => {
                return self.mark_provider_payload_failed(
                    &payload.id,
                    decoder.version,
                    "decode_failed",
                    error,
                );
            }
            Err(panic_payload) => {
                return self.mark_provider_payload_failed(
                    &payload.id,
                    decoder.version,
                    "decode_panicked",
                    panic_message(panic_payload),
                );
            }
        };

        match self
            .sqlite
            .append_decoded_provider_payload(&payload.id, decoder.version, &decoded)
        {
            Ok(()) => Ok(()),
            Err(error) => self.mark_provider_payload_failed(
                &payload.id,
                decoder.version,
                "decode_save_failed",
                error.to_string(),
            ),
        }
    }

    fn provider_payload_recovery_report_with_decoders(
        &self,
        decoders: &[PayloadDecoder],
    ) -> Result<PayloadRecoveryReport, SqliteStoreError> {
        let mut diffs = Vec::new();

        for payload in self.sqlite.list_decoded_provider_payloads()? {
            let Some(decoder) = decoder_for_api_kind(decoders, payload.api_kind.as_str()) else {
                continue;
            };
            if payload.decoder_version.as_deref() == Some(decoder.version) {
                continue;
            }

            let raw_bytes = self
                .read_provider_payload_bytes(&payload)
                .map_err(SqliteStoreError::ReadFailed)?;
            let decoded =
                decode_payload_for_report(decoder, &payload, raw_bytes).map_err(|error| {
                    SqliteStoreError::ReadFailed(format!(
                        "failed to re-decode provider payload {}: {error}",
                        payload.id
                    ))
                })?;
            let stored_parts = self.sqlite.list_parts_for_provider_payload(&payload.id)?;
            let stored_parts_json = parts_json(stored_parts.iter().map(|part| &part.part))?;
            let decoded_parts_json = parts_json(
                decoded
                    .turns
                    .iter()
                    .flat_map(|turn| turn.parts.iter().map(|part| &part.part)),
            )?;

            if stored_parts_json != decoded_parts_json {
                diffs.push(ProviderPayloadDiff {
                    payload_id: payload.id,
                    api_kind: payload.api_kind,
                    stored_decoder_version: payload.decoder_version,
                    current_decoder_version: decoder.version.to_string(),
                    stored_parts_json,
                    decoded_parts_json,
                });
            }
        }

        Ok(PayloadRecoveryReport { diffs })
    }

    fn read_provider_payload_bytes(&self, payload: &ProviderPayloadRow) -> Result<Vec<u8>, String> {
        let mut artifact = self
            .sqlite
            .get_artifact(&payload.artifact_id)
            .map_err(|error| error.to_string())?;
        let mut raw_bytes = Vec::new();
        artifact
            .reader
            .read_to_end(&mut raw_bytes)
            .map_err(|error| error.to_string())?;
        Ok(raw_bytes)
    }

    fn mark_provider_payload_failed(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        kind: &str,
        message: String,
    ) -> Result<(), SqliteStoreError> {
        let error_json = json!({
            "kind": kind,
            "message": message,
        })
        .to_string();
        self.sqlite
            .mark_provider_payload_failed(id, decoder_version, &error_json)
    }

    fn mark_provider_payload_ignored(
        &self,
        id: &ProviderPayloadId,
        kind: &str,
        message: String,
    ) -> Result<(), SqliteStoreError> {
        let reason_json = json!({
            "kind": kind,
            "message": message,
        })
        .to_string();
        self.sqlite.mark_provider_payload_ignored(id, &reason_json)
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::open(ephemeral_db_path()).expect("ephemeral session store should open")
    }
}

fn decoder_for_api_kind<'a>(
    decoders: &'a [PayloadDecoder],
    api_kind: &str,
) -> Option<&'a PayloadDecoder> {
    decoders
        .iter()
        .find(|decoder| decoder.api_kinds.contains(&api_kind))
}

fn decode_openai_chat_completions_payload(
    payload: &ProviderPayloadRow,
    raw_json: Vec<u8>,
) -> Result<DecodedProviderPayload, String> {
    OpenAiChatCompletionsDecoder::new()
        .decode(&OpenAiChatCompletionsDecodeInput {
            provider_payload_id: payload.id.clone(),
            raw_artifact_id: payload.artifact_id.clone(),
            run_id: payload.run_id.clone(),
            provider_id: payload.provider_id.clone(),
            raw_json,
            created_at: payload.created_at,
        })
        .map_err(|error| error.to_string())
}

fn decode_chatgpt_subscription_payload(
    payload: &ProviderPayloadRow,
    raw_json: Vec<u8>,
) -> Result<DecodedProviderPayload, String> {
    ChatGptSubscriptionDecoder::new()
        .decode(&ChatGptSubscriptionDecodeInput {
            provider_payload_id: payload.id.clone(),
            raw_artifact_id: payload.artifact_id.clone(),
            run_id: payload.run_id.clone(),
            provider_id: payload.provider_id.clone(),
            raw_json,
            created_at: payload.created_at,
        })
        .map_err(|error| error.to_string())
}

fn decode_openai_responses_payload(
    payload: &ProviderPayloadRow,
    raw_json: Vec<u8>,
) -> Result<DecodedProviderPayload, String> {
    OpenAiResponsesDecoder::new()
        .decode(&OpenAiResponsesDecodeInput {
            provider_payload_id: payload.id.clone(),
            raw_artifact_id: payload.artifact_id.clone(),
            run_id: payload.run_id.clone(),
            provider_id: payload.provider_id.clone(),
            raw_json,
            created_at: payload.created_at,
        })
        .map_err(|error| error.to_string())
}

fn decode_payload_for_report(
    decoder: &PayloadDecoder,
    payload: &ProviderPayloadRow,
    raw_bytes: Vec<u8>,
) -> Result<DecodedProviderPayload, String> {
    match catch_unwind(AssertUnwindSafe(|| (decoder.decode)(payload, raw_bytes))) {
        Ok(result) => result,
        Err(payload) => Err(format!("decoder panicked: {}", panic_message(payload))),
    }
}

fn parts_json<'a>(parts: impl Iterator<Item = &'a Part>) -> Result<String, SqliteStoreError> {
    let parts = parts.collect::<Vec<_>>();
    serde_json::to_string(&parts).map_err(|error| SqliteStoreError::ReadFailed(error.to_string()))
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "decoder panicked without a string payload".to_string()
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
    use super::super::sqlite::{NewProviderPayload, ProviderPayloadDirection};
    use super::*;

    struct TempDataDir {
        path: PathBuf,
    }

    impl TempDataDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nav-session-store-{name}-{}-{}",
                std::process::id(),
                unix_millis()
            ));
            std::fs::create_dir(&path).expect("temp data dir should be created");
            Self { path }
        }

        fn db_path(&self) -> PathBuf {
            self.path.join("nav.db")
        }
    }

    impl Drop for TempDataDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

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

    #[test]
    fn open_recovers_pending_provider_payloads() {
        let (_data_dir, path, payload_id, run_id) = seed_provider_payload(
            "pending-decode-recovery",
            "openai_chat_completions",
            ProviderPayloadDirection::Response,
            br#"{"id":"chatcmpl_1","choices":[{"message":{"content":"recovered"}}]}"#.to_vec(),
        );

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "decoded");
        assert_eq!(
            payload.decoder_version.as_deref(),
            Some(OPENAI_CHAT_COMPLETIONS_DECODER_VERSION)
        );

        let turns = store
            .sqlite
            .list_turns_for_run(&run_id)
            .expect("decoded turns should be readable");
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].1[0].part,
            Part::Text {
                text: "recovered".to_string(),
                synthetic: None,
            }
        );
    }

    #[test]
    fn open_recovers_chatgpt_subscription_payload_and_keeps_raw_event_order() {
        let raw_json = r#"{"events":[{"type":"response.created","response":{"id":"resp_sub_1","model":"gpt-5.1-codex"}},{"type":"response.output_item.done","output_index":0,"item":{"id":"rs_1","type":"reasoning","encrypted_content":"enc_reasoning_1"}},{"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"hello "},{"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Season"},{"type":"response.output_text.done","output_index":1,"content_index":0,"text":"hello Season"},{"type":"response.completed","response":{"id":"resp_sub_1","model":"gpt-5.1-codex","status":"completed"}}]}"#;
        let (_data_dir, path, payload_id, run_id) = seed_provider_payload(
            "pending-chatgpt-subscription-recovery",
            "chatgpt-subscription",
            ProviderPayloadDirection::StreamBatch,
            raw_json.as_bytes().to_vec(),
        );

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "decoded");
        assert_eq!(
            payload.decoder_version.as_deref(),
            Some(CHATGPT_SUBSCRIPTION_DECODER_VERSION)
        );

        let turns = store
            .sqlite
            .list_turns_for_run(&run_id)
            .expect("decoded turns should be readable");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].1.len(), 2);
        assert_eq!(
            turns[0].1[0].provider_json_pointer.as_deref(),
            Some("/events/1/item/encrypted_content")
        );
        assert_eq!(
            turns[0].1[1].provider_json_pointer.as_deref(),
            Some("/events/4/text")
        );

        let mut artifact = store
            .sqlite
            .get_artifact(&payload.artifact_id)
            .expect("raw payload artifact should be readable");
        let mut recovered_raw = String::new();
        artifact
            .reader
            .read_to_string(&mut recovered_raw)
            .expect("raw payload artifact should be utf-8");
        assert_eq!(recovered_raw, raw_json);
    }

    #[test]
    fn open_recovers_pending_provider_payloads_with_unknowns() {
        let (_data_dir, path, payload_id, run_id) = seed_provider_payload(
            "pending-decode-recovery-unknowns",
            "openai_chat_completions",
            ProviderPayloadDirection::Response,
            br#"{"id":"chatcmpl_1","choices":[{"message":{"content":"recovered","vendor_extra":{"nested":[true,false]}}}]}"#.to_vec(),
        );

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "decoded_with_unknowns");

        let turns = store
            .sqlite
            .list_turns_for_run(&run_id)
            .expect("decoded turns should be readable");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].1.len(), 2);
        assert_eq!(
            turns[0].1[1].provider_json_pointer.as_deref(),
            Some("/choices/0/message/vendor_extra")
        );
    }

    #[test]
    fn open_recovers_pending_openai_responses_payloads() {
        let (_data_dir, path, payload_id, run_id) = seed_provider_payload(
            "pending-responses-decode-recovery",
            "openai-responses",
            ProviderPayloadDirection::Response,
            br#"{"id":"resp_1","status":"completed","model":"gpt-5.1","output":[{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"recovered responses","annotations":[]}]}]}"#.to_vec(),
        );

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "decoded");
        assert_eq!(
            payload.decoder_version.as_deref(),
            Some(OPENAI_RESPONSES_DECODER_VERSION)
        );

        let turns = store
            .sqlite
            .list_turns_for_run(&run_id)
            .expect("decoded turns should be readable");
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].1[1].part,
            Part::Text {
                text: "recovered responses".to_string(),
                synthetic: None,
            }
        );
    }

    #[test]
    fn panicking_decoder_marks_payload_failed_without_retry_loop() {
        let (_data_dir, path, payload_id, _run_id) = seed_provider_payload(
            "panic-decode-recovery",
            "panic_api",
            ProviderPayloadDirection::Response,
            br#"{"ok":true}"#.to_vec(),
        );
        let store = SessionStore {
            sqlite: SqliteSessionStore::open(&path).expect("recovery open should succeed"),
        };
        let panic_decoder = PayloadDecoder {
            api_kinds: &["panic_api"],
            version: "panic-decoder@1",
            decode: decode_with_panic,
        };

        let recovered = store
            .recover_pending_provider_payloads_with_decoders(&[panic_decoder])
            .expect("recovery should convert panic into failed status");

        assert_eq!(recovered, 1);
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "failed");
        assert_eq!(payload.decoder_version.as_deref(), Some("panic-decoder@1"));
        assert!(payload.error_json.unwrap().contains("decode_panicked"));

        let retried = store
            .recover_pending_provider_payloads_with_decoders(&[panic_decoder])
            .expect("failed payload should not be pending");
        assert_eq!(retried, 0);
    }

    #[test]
    fn decoded_save_failure_marks_payload_failed_without_retry_loop() {
        let (_data_dir, path, payload_id, _run_id) = seed_provider_payload(
            "save-failed-decode-recovery",
            "bad_save_api",
            ProviderPayloadDirection::Response,
            br#"{"ok":true}"#.to_vec(),
        );
        let store = SessionStore {
            sqlite: SqliteSessionStore::open(&path).expect("recovery open should succeed"),
        };
        let bad_save_decoder = PayloadDecoder {
            api_kinds: &["bad_save_api"],
            version: "bad-save-decoder@1",
            decode: decode_with_mismatched_payload_id,
        };

        let recovered = store
            .recover_pending_provider_payloads_with_decoders(&[bad_save_decoder])
            .expect("recovery should convert save failure into failed status");

        assert_eq!(recovered, 1);
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "failed");
        assert_eq!(
            payload.decoder_version.as_deref(),
            Some("bad-save-decoder@1")
        );
        assert!(payload.error_json.unwrap().contains("decode_save_failed"));

        let retried = store
            .recover_pending_provider_payloads_with_decoders(&[bad_save_decoder])
            .expect("failed payload should not be pending");
        assert_eq!(retried, 0);
    }

    #[test]
    fn pending_request_payloads_are_ignored_on_recovery() {
        let (_data_dir, path, payload_id, _run_id) = seed_provider_payload(
            "pending-request-recovery",
            "openai_chat_completions",
            ProviderPayloadDirection::Request,
            br#"{"messages":[]}"#.to_vec(),
        );
        SqliteSessionStore::open(&path)
            .expect("setup reopen should succeed")
            .execute_write(|tx| {
                tx.execute(
                    "UPDATE provider_payloads SET decode_status = 'pending', decoded_at = NULL WHERE id = ?1",
                    [payload_id.as_str()],
                )
            })
            .expect("fixture should force old pending request state");

        let store = SessionStore::open(&path).expect("open should recover pending payloads");
        let payload = store
            .sqlite
            .get_provider_payload(&payload_id)
            .expect("payload should be readable after recovery");
        assert_eq!(payload.decode_status, "ignored");
        assert!(payload.error_json.unwrap().contains("request_payload"));
    }

    fn session_id() -> SessionId {
        SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap()
    }

    fn run_id() -> RunId {
        RunId::try_new("019f2f6f-f178-7a72-9f28-000000000002").unwrap()
    }

    fn seed_provider_payload(
        name: &str,
        api_kind: &str,
        direction: ProviderPayloadDirection,
        raw_bytes: Vec<u8>,
    ) -> (TempDataDir, PathBuf, ProviderPayloadId, RunId) {
        let data_dir = TempDataDir::new(name);
        let path = data_dir.db_path();
        let session_id = session_id();
        let run_id = run_id();
        let setup = SqliteSessionStore::open(&path).expect("setup open should succeed");
        setup
            .create_session(session_id.clone(), default_session_record())
            .expect("session create should commit");
        setup
            .start_run(StartRun {
                id: run_id.clone(),
                session_id: session_id.clone(),
                status: RunStatus::Running,
                trigger: Some("test".to_string()),
                started_at: 2_000,
            })
            .expect("run start should commit");
        let payload_id = setup
            .append_provider_payload(NewProviderPayload {
                session_id,
                run_id: run_id.clone(),
                direction,
                api_kind: api_kind.to_string(),
                provider_id: Some("test".to_string()),
                model_id: Some("test-model".to_string()),
                sequence: 0,
                provider_payload_id: Some("provider_payload".to_string()),
                mime: "application/json".to_string(),
                raw_bytes,
                created_at: 3_000,
            })
            .expect("provider payload append should commit");

        (data_dir, path, payload_id, run_id)
    }

    fn decode_with_panic(
        _payload: &ProviderPayloadRow,
        _raw_json: Vec<u8>,
    ) -> Result<DecodedProviderPayload, String> {
        panic!("decoder exploded")
    }

    fn decode_with_mismatched_payload_id(
        payload: &ProviderPayloadRow,
        _raw_json: Vec<u8>,
    ) -> Result<DecodedProviderPayload, String> {
        Ok(DecodedProviderPayload {
            status: super::super::sqlite::DecodeStatus::Decoded,
            turns: vec![crate::models::DecodedTurn {
                turn: Turn {
                    id: MessageId::new_unchecked("019f2f6f-f178-7a72-9f28-000000000052"),
                    run_id: payload.run_id.clone(),
                    seq: 0,
                    role: TurnRole::Assistant,
                    meta: TurnMeta::default(),
                    created_at: payload.created_at,
                },
                parts: vec![crate::models::DecodedPart {
                    part: Part::Text {
                        text: "bad save".to_string(),
                        synthetic: None,
                    },
                    provider_payload_id: ProviderPayloadId::new_unchecked("bad_payload_id"),
                    provider_json_pointer: "/choices/0/message/content".to_string(),
                }],
            }],
        })
    }
}
