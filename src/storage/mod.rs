//! Durable session storage backed by the shared `~/.nav/nav.db` SQLite database.
//!
//! Every chat exchange — the session, each run, and the user/assistant turns
//! with their text parts — is persisted into the existing schema (also written
//! by the pi CLI, which additionally stores raw provider payloads). nav treats
//! that structure as a fixed contract: it only inserts and updates rows, never
//! alters tables. On a database that has no tables yet, nav applies the
//! captured schema once (migration version 1); an existing database is used
//! as-is.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::model::{ChatMessage, ToolCall};

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

/// A persistent store of chat sessions over the shared SQLite database.
pub struct Storage {
    conn: Mutex<Connection>,
}

/// A session as shown in the sidebar listing: its id, a short title drawn from
/// the first user message, and when it was last active.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
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
        // WAL lets nav and the pi CLI read/write the shared database concurrently.
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
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, source, settings_json, version, created_at, updated_at)
             VALUES (?1, ?2, '{}', ?3, ?4, ?4)",
            params![session_id, source, env!("CARGO_PKG_VERSION"), now],
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
        self.record_turn(TurnRecord {
            session_id,
            run_id,
            seq,
            role: "assistant",
            model_id,
            meta_json: assistant_meta(model_id),
            parts: vec![text_part(text)],
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
        let mut parts = Vec::with_capacity(calls.len() + 1);
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
            "SELECT s.id, s.updated_at,
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
            Ok(SessionSummary {
                id: row.get(0)?,
                updated_at: row.get(1)?,
                title: row.get::<_, Option<String>>(2)?,
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

    /// Replay a session's text turns in order, so a session can be resumed with
    /// its prior conversation context intact across restarts.
    ///
    /// One message per turn: a turn's text parts are concatenated (a turn may
    /// hold several parts in the shared schema), and turns are ordered by run
    /// start then sequence, with the run id as a stable tiebreaker.
    pub fn load_history(&self, session_id: &str) -> Result<Vec<ChatMessage>, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT t.role,
                    (SELECT group_concat(json_extract(tp.data_json, '$.text'), '')
                     FROM turn_parts tp
                     WHERE tp.turn_id = t.id AND tp.type = 'text') AS text
             FROM turns t
             JOIN runs r ON r.id = t.run_id
             WHERE r.session_id = ?1
               AND EXISTS (
                   SELECT 1 FROM turn_parts tp2
                   WHERE tp2.turn_id = t.id AND tp2.type = 'text'
               )
             ORDER BY r.started_at, r.id, t.seq",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            let role: String = row.get(0)?;
            let text: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            Ok((role, text))
        })?;

        let mut history = Vec::new();
        for row in rows {
            let (role, text) = row?;
            let message = match role.as_str() {
                "assistant" => ChatMessage::assistant(text),
                _ => ChatMessage::user(text),
            };
            history.push(message);
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

/// A single `text` part wrapping `text`.
fn text_part(text: &str) -> TurnPart {
    TurnPart {
        part_type: "text",
        data_json: serde_json::json!({ "type": "text", "text": text }).to_string(),
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

    // A non-empty database must already be a nav/pi database. If the expected
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
                "database is not a nav/pi database (missing table '{table}'); refusing to modify it"
            )));
        }
    }
    Ok(())
}

fn new_id() -> String {
    Uuid::now_v7().to_string()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
