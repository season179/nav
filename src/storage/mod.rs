//! Durable session storage backed by the `~/.nav/nav.db` SQLite database.
//!
//! Every chat exchange — the session, each run, and the user/assistant turns
//! with their text parts — is persisted into this schema, which nav adopted
//! verbatim from a pre-existing `~/.nav/nav.db` and also carries raw provider
//! payloads. nav treats that structure as a fixed contract: it only inserts
//! and updates rows, never alters tables. On a database that has no tables
//! yet, nav applies the captured schema once (migration version 1); an
//! existing database (which may hold rows from the tool that created it) is
//! used as-is.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::context::TurnHistory;
use crate::model::{ChatMessage, ResponseReasoningItem, ToolCall};
use crate::tokens::TokenUsage;

/// Canonical schema, captured verbatim from the live database.
const SCHEMA: &str = include_str!("schema.sql");
const SCHEMA_VERSION: i64 = 1;
/// The only provider API nav speaks; recorded on assistant turns.
const API_KIND: &str = "openai-completions";

/// Why a storage operation failed. Persistence problems are surfaced to the
/// backend (logged) but never crash an in-flight chat.
#[derive(Debug)]
pub struct StorageError(pub String);

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session storage error: {}", self.0)
    }
}

impl std::error::Error for StorageError {}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        StorageError(error.to_string())
    }
}

/// A persistent store of chat sessions over the `~/.nav/nav.db` SQLite database.
pub struct Storage {
    conn: Mutex<Connection>,
}

/// A session as shown in the sidebar listing: its project workspace, id, a short
/// title drawn from the first user message, and when it was last active.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    pub workspace_root: Option<String>,
    pub project_root: Option<String>,
    pub updated_at: i64,
}

impl Storage {
    /// Open (creating and migrating when empty) the session database at `path`.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StorageError(format!("cannot create {}: {e}", parent.display())))?;
        }

        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        // WAL allows concurrent readers/writers (e.g. several nav processes) without lock contention.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        ensure_schema(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open the default database at `~/.nav/nav.db`.
    pub fn open_default() -> Result<Self, StorageError> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| StorageError("cannot determine home directory".to_owned()))?;
        Self::open(&PathBuf::from(home).join(".nav").join("nav.db"))
    }

    /// Record a freshly created chat session.
    pub fn create_session(&self, session_id: &str, source: &str) -> Result<(), StorageError> {
        self.create_session_with_workspace(session_id, source, None)
    }

    /// Record a freshly created chat session, including the workspace it belongs to.
    pub fn create_session_with_workspace(
        &self,
        session_id: &str,
        source: &str,
        workspace_root: Option<&Path>,
    ) -> Result<(), StorageError> {
        let now = now_ms();
        let workspace_root = workspace_root_string(workspace_root);
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, source, workspace_root, settings_json, version, created_at, updated_at)
             VALUES (?1, ?2, ?3, '{}', ?4, ?5, ?5)",
            params![
                session_id,
                source,
                workspace_root,
                env!("CARGO_PKG_VERSION"),
                now
            ],
        )?;
        Ok(())
    }

    /// Open a run for one chat turn (status `running`).
    pub fn start_run(&self, run_id: &str, session_id: &str) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO runs (id, session_id, status, trigger, started_at)
             VALUES (?1, ?2, 'running', 'session.sendMessage', ?3)",
            params![run_id, session_id, now_ms()],
        )?;
        Ok(())
    }

    /// Persist the user's message as turn `seq` (a single text part).
    pub fn record_user_text(
        &self,
        session_id: &str,
        run_id: &str,
        seq: i64,
        text: &str,
    ) -> Result<(), StorageError> {
        self.record_turn(TurnRecord {
            session_id,
            run_id,
            seq,
            role: "user",
            model_id: None,
            meta_json: "{}".to_owned(),
            parts: vec![text_part(text)],
        })
    }

    /// Persist the assistant's reply as turn `seq` (a single text part), tagging
    /// it with the model that produced it.
    pub fn record_assistant_text(
        &self,
        session_id: &str,
        run_id: &str,
        seq: i64,
        text: &str,
        model_id: Option<&str>,
    ) -> Result<(), StorageError> {
        self.record_assistant_text_with_reasoning(
            session_id,
            run_id,
            seq,
            (text, None, &[]),
            model_id,
        )
    }

    /// Persist the assistant's reply, preserving provider reasoning/thinking
    /// content when the active model requires it for replay.
    pub fn record_assistant_text_with_reasoning(
        &self,
        session_id: &str,
        run_id: &str,
        seq: i64,
        content: (&str, Option<&str>, &[ResponseReasoningItem]),
        model_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let (text, reasoning_content, response_reasoning_items) = content;
        let mut parts = Vec::with_capacity(
            1 + usize::from(reasoning_content.is_some() || !response_reasoning_items.is_empty()),
        );
        if reasoning_content.is_some() || !response_reasoning_items.is_empty() {
            parts.push(thinking_part(reasoning_content, response_reasoning_items));
        }
        parts.push(text_part(text));
        self.record_turn(TurnRecord {
            session_id,
            run_id,
            seq,
            role: "assistant",
            model_id,
            meta_json: assistant_meta(model_id),
            parts,
        })
    }

    /// Persist an assistant turn that requested tool calls: an optional text
    /// part plus one `tool_call` part per call. `tool_call` parts are not
    /// mirrored to the FTS index (by design — they aren't free text).
    pub fn record_assistant_tool_calls(
        &self,
        session_id: &str,
        run_id: &str,
        seq: i64,
        content: Option<&str>,
        calls: &[ToolCall],
        model_id: Option<&str>,
    ) -> Result<(), StorageError> {
        self.record_assistant_tool_calls_with_reasoning(
            session_id,
            run_id,
            seq,
            (content, None, &[]),
            calls,
            model_id,
        )
    }

    /// Persist an assistant tool-call turn while preserving provider
    /// reasoning/thinking content for future replay.
    pub fn record_assistant_tool_calls_with_reasoning(
        &self,
        session_id: &str,
        run_id: &str,
        seq: i64,
        content: (Option<&str>, Option<&str>, &[ResponseReasoningItem]),
        calls: &[ToolCall],
        model_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let (content, reasoning_content, response_reasoning_items) = content;
        let mut parts = Vec::with_capacity(
            calls.len()
                + usize::from(content.is_some_and(|text| !text.is_empty()))
                + usize::from(reasoning_content.is_some() || !response_reasoning_items.is_empty()),
        );
        if reasoning_content.is_some() || !response_reasoning_items.is_empty() {
            parts.push(thinking_part(reasoning_content, response_reasoning_items));
        }
        if let Some(content) = content.filter(|text| !text.is_empty()) {
            parts.push(text_part(content));
        }
        for call in calls {
            parts.push(TurnPart {
                part_type: "tool_call",
                data_json: serde_json::json!({
                    "type": "tool_call",
                    "id": call.id,
                    "name": call.name,
                    "arguments": call.arguments,
                })
                .to_string(),
            });
        }

        self.record_turn(TurnRecord {
            session_id,
            run_id,
            seq,
            role: "assistant",
            model_id,
            meta_json: assistant_meta(model_id),
            parts,
        })
    }

    /// Persist a tool result as a `tool`-role turn with a single `tool_result`
    /// part. The result text lives under `data_json.content` so the existing FTS
    /// trigger mirrors it into search.
    pub fn record_tool_result(
        &self,
        session_id: &str,
        run_id: &str,
        seq: i64,
        tool_call_id: &str,
        content: &str,
        is_error: bool,
    ) -> Result<(), StorageError> {
        let part = TurnPart {
            part_type: "tool_result",
            data_json: serde_json::json!({
                "type": "tool_result",
                "tool_call_id": tool_call_id,
                "content": content,
                "is_error": is_error,
            })
            .to_string(),
        };
        self.record_turn(TurnRecord {
            session_id,
            run_id,
            seq,
            role: "tool",
            model_id: None,
            meta_json: "{}".to_owned(),
            parts: vec![part],
        })
    }

    /// Mark a run completed.
    pub fn complete_run(&self, run_id: &str) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE runs SET status = 'completed', finished_at = ?2 WHERE id = ?1",
            params![run_id, now_ms()],
        )?;
        Ok(())
    }

    /// Mark a run failed, recording the error message.
    pub fn fail_run(&self, run_id: &str, error: &str) -> Result<(), StorageError> {
        let error_json = serde_json::json!({ "message": error }).to_string();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE runs SET status = 'failed', finished_at = ?2, error_json = ?3 WHERE id = ?1",
            params![run_id, now_ms(), error_json],
        )?;
        Ok(())
    }

    /// Mark a run cancelled after a user-requested stop.
    pub fn cancel_run(&self, run_id: &str) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE runs SET status = 'cancelled', finished_at = ?2 WHERE id = ?1",
            params![run_id, now_ms()],
        )?;
        Ok(())
    }

    /// Add observed or estimated token counts to the session aggregate.
    ///
    /// The schema stores these on `sessions` rather than per run. nav
    /// records them as operational telemetry for future context management, not
    /// as billing data.
    pub fn record_token_usage(
        &self,
        session_id: &str,
        usage: &TokenUsage,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions
             SET tokens_input = tokens_input + ?2,
                 tokens_output = tokens_output + ?3,
                 tokens_reasoning = tokens_reasoning + ?4,
                 tokens_cache_read = tokens_cache_read + ?5,
                 tokens_cache_write = tokens_cache_write + ?6
             WHERE id = ?1",
            params![
                session_id,
                sqlite_i64(usage.input),
                sqlite_i64(usage.output),
                sqlite_i64(usage.reasoning),
                sqlite_i64(usage.cache_read),
                sqlite_i64(usage.cache_write),
            ],
        )?;
        Ok(())
    }

    /// All sessions from `source`, most-recently-updated first, for listing in
    /// the sidebar. `title` is the session's first user message (callers
    /// truncate and supply a fallback for empty sessions).
    pub fn list_sessions(&self, source: &str) -> Result<Vec<SessionSummary>, StorageError> {
        let conn = self.conn.lock().unwrap();
        // The title reads from the derived `turn_parts_text` table (kept in sync
        // by triggers) rather than scanning `turn_parts` and json_extract-ing
        // each row: its `idx_turn_parts_text_turn_id` index turns the per-session
        // lookup into an indexed join. `turn_parts_text` already drops empty
        // parts, so the first non-empty user text wins.
        let mut stmt = conn.prepare(
            "SELECT s.id, s.updated_at, s.workspace_root,
                    (SELECT tpt.text
                     FROM turn_parts_text tpt
                     JOIN turns t ON t.id = tpt.turn_id
                     JOIN runs r ON r.id = t.run_id
                     WHERE r.session_id = s.id AND t.role = 'user' AND tpt.part_type = 'text'
                     ORDER BY r.started_at, t.seq, tpt.part_id
                     LIMIT 1) AS title
             FROM sessions s
             WHERE s.source = ?1
             ORDER BY s.updated_at DESC, s.created_at DESC, s.id DESC",
        )?;
        let rows = stmt.query_map(params![source], |row| {
            let workspace_root = row
                .get::<_, Option<String>>(2)?
                .and_then(stored_workspace_root_string);
            Ok(SessionSummary {
                id: row.get(0)?,
                updated_at: row.get(1)?,
                project_root: workspace_root
                    .as_deref()
                    .map(|path| project_root_to_string(Path::new(path))),
                workspace_root,
                title: row.get::<_, Option<String>>(3)?,
            })
        })?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// The id of the most recently active session from `source`, if any. Used
    /// to reopen the last conversation when the app restarts.
    pub fn most_recent_session(&self, source: &str) -> Result<Option<String>, StorageError> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT id FROM sessions WHERE source = ?1
                 ORDER BY updated_at DESC, created_at DESC, id DESC LIMIT 1",
                params![source],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    /// The most recently active session from `source` in `workspace_root`, if
    /// any. Blank legacy workspace roots only match a blank requested workspace.
    pub fn most_recent_session_in_workspace(
        &self,
        source: &str,
        workspace_root: &Path,
    ) -> Result<Option<String>, StorageError> {
        let workspace_root = workspace_root_string(Some(workspace_root)).unwrap_or_default();
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT id FROM sessions
                 WHERE source = ?1
                   AND (
                     (NULLIF(TRIM(workspace_root), '') IS NULL AND ?2 = '')
                     OR NULLIF(TRIM(workspace_root), '') = ?2
                   )
                 ORDER BY updated_at DESC, created_at DESC, id DESC LIMIT 1",
                params![source, workspace_root],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Whether a session with this id is already persisted.
    pub fn session_exists(&self, session_id: &str) -> Result<bool, StorageError> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The workspace root recorded for a persisted session, if any.
    pub fn session_workspace_root(&self, session_id: &str) -> Result<Option<String>, StorageError> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT workspace_root FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .and_then(stored_workspace_root_string))
    }

    /// Rebuild a Session's Turn History in order, so it can be resumed with its
    /// prior conversation — text *and* tool calls/results — intact across
    /// restarts.
    ///
    /// Every part of every turn is read and grouped back into messages: a user
    /// or assistant turn's text parts are concatenated; an assistant turn's
    /// `tool_call` parts rebuild its requested calls; a `tool` turn's
    /// `tool_result` parts each become a tool-result message. Turns are ordered
    /// by run start then sequence (run id as a stable tiebreaker), and a turn's
    /// parts by creation, so the rebuilt Turn History matches what was recorded.
    pub fn load_history(&self, session_id: &str) -> Result<TurnHistory, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT t.id, t.role, tp.type, tp.data_json
             FROM turns t
             JOIN runs r ON r.id = t.run_id
             JOIN turn_parts tp ON tp.turn_id = t.id
             WHERE r.session_id = ?1
             ORDER BY r.started_at, r.id, t.seq, tp.created_at, tp.id",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;

        // The ordering keeps every part of a turn contiguous, so accumulate the
        // parts of the current turn and flush a turn's messages when the next
        // turn id appears.
        let mut history = TurnHistory::new();
        let mut current: Option<TurnAccum> = None;
        for row in rows {
            let (turn_id, role, part_type, data_json) = row?;
            let mut acc = match current.take() {
                Some(acc) if acc.turn_id == turn_id => acc,
                finished => {
                    if let Some(turn) = finished {
                        turn.flush(&mut history);
                    }
                    TurnAccum::new(turn_id, role)
                }
            };
            acc.push_part(&part_type, &data_json);
            current = Some(acc);
        }
        if let Some(acc) = current {
            acc.flush(&mut history);
        }
        Ok(history)
    }

    fn record_turn(&self, turn: TurnRecord) -> Result<(), StorageError> {
        let turn_id = new_id();
        let now = now_ms();

        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO turns (id, run_id, seq, role, meta_json, created_at, model_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                turn_id,
                turn.run_id,
                turn.seq,
                turn.role,
                turn.meta_json,
                now,
                turn.model_id
            ],
        )?;
        // One row per part. The turn_parts insert trigger mirrors text and
        // tool_result parts into turn_parts_text and the FTS indexes; tool_call
        // parts are intentionally not mirrored.
        for part in &turn.parts {
            tx.execute(
                "INSERT INTO turn_parts (id, turn_id, session_id, type, data_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    new_id(),
                    turn_id,
                    turn.session_id,
                    part.part_type,
                    part.data_json,
                    now
                ],
            )?;
        }
        // Keep the session's updated_at fresh so listings sort sensibly.
        tx.execute(
            "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
            params![turn.session_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }
}

/// The fields of one persisted turn, grouped to keep [`Storage::record_turn`]
/// to a single argument.
struct TurnRecord<'a> {
    session_id: &'a str,
    run_id: &'a str,
    seq: i64,
    role: &'a str,
    model_id: Option<&'a str>,
    meta_json: String,
    parts: Vec<TurnPart>,
}

/// One persisted part of a turn: its `turn_parts.type` and `data_json` payload.
struct TurnPart {
    part_type: &'static str,
    data_json: String,
}

/// Accumulates the parts of one turn while [`Storage::load_history`] streams
/// them, then flushes the reconstructed [`ChatMessage`]s for that turn.
struct TurnAccum {
    turn_id: String,
    role: String,
    /// Concatenated `text` parts (a turn may hold several).
    text: String,
    /// Provider reasoning/thinking payload from `thinking` parts.
    reasoning_content: Option<String>,
    /// Opaque Responses API reasoning payloads from `thinking` parts.
    response_reasoning_items: Vec<ResponseReasoningItem>,
    /// `tool_call` parts rebuilt into the assistant's requested calls.
    tool_calls: Vec<ToolCall>,
    /// `tool_result` parts as `(tool_call_id, content, is_error)`.
    tool_results: Vec<(String, String, bool)>,
}

impl TurnAccum {
    fn new(turn_id: String, role: String) -> Self {
        Self {
            turn_id,
            role,
            text: String::new(),
            reasoning_content: None,
            response_reasoning_items: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    /// Fold one part into the turn. Unknown or malformed parts are skipped
    /// rather than failing the whole resume — a single bad part shouldn't make
    /// a session unopenable.
    fn push_part(&mut self, part_type: &str, data_json: &str) {
        let Ok(data) = serde_json::from_str::<serde_json::Value>(data_json) else {
            return;
        };
        let field = |key: &str| data.get(key).and_then(|v| v.as_str()).unwrap_or_default();
        match part_type {
            "text" => self.text.push_str(field("text")),
            "thinking" => {
                let text = field("text");
                if !text.is_empty() {
                    match &mut self.reasoning_content {
                        Some(reasoning_content) => reasoning_content.push_str(text),
                        None => self.reasoning_content = Some(text.to_owned()),
                    }
                }
                self.response_reasoning_items
                    .extend(response_reasoning_items_from_part(&data));
            }
            "tool_call" => self.tool_calls.push(ToolCall {
                id: field("id").to_owned(),
                name: field("name").to_owned(),
                arguments: field("arguments").to_owned(),
            }),
            "tool_result" => self.tool_results.push((
                field("tool_call_id").to_owned(),
                field("content").to_owned(),
                data.get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            )),
            _ => {}
        }
    }

    /// Append this turn's reconstructed message(s) to the rebuilt Turn History.
    fn flush(self, history: &mut TurnHistory) {
        match self.role.as_str() {
            "assistant" if !self.tool_calls.is_empty() => {
                let mut message = ChatMessage::assistant_tool_calls(self.text, self.tool_calls);
                message.reasoning_content = self.reasoning_content;
                message.response_reasoning_items = self.response_reasoning_items;
                history.push(message);
            }
            "assistant" => {
                let mut message = ChatMessage::assistant(self.text);
                message.reasoning_content = self.reasoning_content;
                message.response_reasoning_items = self.response_reasoning_items;
                history.push(message);
            }
            "tool" => {
                for (tool_call_id, content, is_error) in self.tool_results {
                    history.push(ChatMessage::tool_result(tool_call_id, content, is_error));
                }
            }
            _ => history.push(ChatMessage::user(self.text)),
        }
    }
}

/// A single `text` part wrapping `text`.
fn text_part(text: &str) -> TurnPart {
    TurnPart {
        part_type: "text",
        data_json: serde_json::json!({ "type": "text", "text": text }).to_string(),
    }
}

fn response_reasoning_items_from_part(data: &serde_json::Value) -> Vec<ResponseReasoningItem> {
    data.get("response_reasoning")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(ResponseReasoningItem {
                id: item.get("id")?.as_str()?.to_owned(),
                encrypted_content: item.get("encrypted_content")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

/// A provider reasoning/thinking part. The schema mirrors `text` into
/// FTS, while `response_reasoning` stays opaque and is only used for replay.
fn thinking_part(
    text: Option<&str>,
    response_reasoning_items: &[ResponseReasoningItem],
) -> TurnPart {
    TurnPart {
        part_type: "thinking",
        data_json: serde_json::json!({
            "type": "thinking",
            "text": text.unwrap_or_default(),
            "provider_hint": "reasoning_content",
            "response_reasoning": response_reasoning_items,
        })
        .to_string(),
    }
}

/// `turns.meta_json` for an assistant turn, tagging the producing model.
fn assistant_meta(model_id: Option<&str>) -> String {
    match model_id {
        Some(id) => serde_json::json!({ "model_id": id, "api_kind": API_KIND }).to_string(),
        None => "{}".to_owned(),
    }
}

fn ensure_schema(conn: &Connection) -> Result<(), StorageError> {
    // Distinguish a truly empty database from a populated one. Only the former
    // gets bootstrapped; we never scribble our tables into an unrelated DB.
    let user_tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;

    if user_tables == 0 {
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![SCHEMA_VERSION, now_ms()],
        )?;
        return Ok(());
    }

    // A non-empty database must already use nav's schema. If the expected
    // tables are missing, it belongs to something else — refuse to modify it.
    for table in ["sessions", "schema_migrations"] {
        let present = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !present {
            return Err(StorageError(format!(
                "database does not use nav's schema (missing table '{table}'); refusing to modify it"
            )));
        }
    }
    Ok(())
}

fn new_id() -> String {
    Uuid::now_v7().to_string()
}

pub(crate) fn workspace_root_to_string(workspace_root: &Path) -> String {
    workspace_root.to_string_lossy().replace('\\', "/")
}

pub(crate) fn project_root_to_string(workspace_root: &Path) -> String {
    canonical_git_worktree_root(workspace_root)
        .unwrap_or_else(|| workspace_root.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

fn workspace_root_string(workspace_root: Option<&Path>) -> Option<String> {
    workspace_root
        .map(workspace_root_to_string)
        .filter(|path| !path.trim().is_empty())
}

fn stored_workspace_root_string(workspace_root: String) -> Option<String> {
    let trimmed = workspace_root.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(workspace_root_to_string(Path::new(trimmed)))
    }
}

/// Return the main checkout root for a linked git worktree, or `None` for a
/// regular checkout/non-git path. Reads `<workspace_root>/.git`, resolves its
/// `gitdir` relative to `workspace_root` when needed, then resolves that git
/// directory's `commondir`; for example `.git` containing
/// `gitdir: /repo/.git/worktrees/nav` plus `commondir` of `../..` returns
/// `Some(/repo)`.
fn canonical_git_worktree_root(workspace_root: &Path) -> Option<PathBuf> {
    let git_file = std::fs::read_to_string(workspace_root.join(".git")).ok()?;
    let git_dir = parse_gitdir(&git_file)?;
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        workspace_root.join(git_dir)
    };
    let common_dir = common_git_dir(&git_dir)?;

    if common_dir.file_name().is_some_and(|name| name == ".git") {
        common_dir.parent().map(PathBuf::from)
    } else {
        None
    }
}

/// Parse a `gitdir: <path>` line from a gitfile and return its path, preserving
/// whether it is relative or absolute. Empty values and files without a
/// `gitdir:` line return `None`; for example `gitdir: ../repo/.git/worktrees/a`
/// returns `Some("../repo/.git/worktrees/a")`.
fn parse_gitdir(git_file: &str) -> Option<PathBuf> {
    git_file.lines().find_map(|line| {
        let value = line.trim().strip_prefix("gitdir:")?.trim();
        if value.is_empty() {
            None
        } else {
            Some(PathBuf::from(value))
        }
    })
}

/// Resolve the common git directory for a linked worktree git dir. A non-empty
/// `commondir` file wins: relative values are joined to `git_dir`, absolute
/// values are used as-is, and the result is canonicalized when possible or
/// lexically normalized when the target is missing. Without `commondir`, this
/// falls back to detecting `<repo>/.git/worktrees/<name>` and returning
/// `<repo>/.git`; otherwise it returns `None`.
/// Example: `git_dir=/repo/.git/worktrees/nav`, `commondir=../..` returns
/// `/repo/.git`.
fn common_git_dir(git_dir: &Path) -> Option<PathBuf> {
    if let Ok(common_dir) = std::fs::read_to_string(git_dir.join("commondir")) {
        let common_dir = common_dir.lines().next()?.trim();
        if !common_dir.is_empty() {
            let common_dir = PathBuf::from(common_dir);
            let common_dir = if common_dir.is_absolute() {
                common_dir
            } else {
                git_dir.join(common_dir)
            };
            return Some(match std::fs::canonicalize(&common_dir) {
                Ok(path) => path,
                Err(_) => normalize_absolute_path(&common_dir),
            });
        }
    }

    let worktrees_dir = git_dir.parent()?;
    if worktrees_dir
        .file_name()
        .is_some_and(|name| name == "worktrees")
    {
        worktrees_dir.parent().map(PathBuf::from)
    } else {
        None
    }
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }

    normalized
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn sqlite_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
