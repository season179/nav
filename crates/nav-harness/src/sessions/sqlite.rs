//! SQLite-backed session storage: connection setup and write concurrency.
//!
//! This module owns the low-level database concerns shared by every higher
//! layer: opening the connection with the agreed pragmas, falling back from
//! WAL to a rollback journal on network filesystems, and serialising writes
//! through `BEGIN IMMEDIATE` with jittered retry to avoid convoy effects.
//! Artifact blob persistence and canonical turn/part persistence also live here.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use nav_types::{
    ArtifactId, ArtifactRow, MessageId, PartId, ProviderPayloadId, ProviderPayloadRow, RunId,
    RunRow, SessionId, SessionRow, StorageCursor,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use crate::models::{DecodedProviderPayload, DecodedTurn};

use super::canonical::{Part, TokenUsage, Turn, TurnRole};
use super::migrate;

/// Maximum number of `BEGIN IMMEDIATE` retries before giving up on a busy DB.
const MAX_WRITE_RETRIES: u32 = 15;
/// Retry backoff is uniform jitter in `[JITTER_MIN_MS, JITTER_MAX_MS]`.
const JITTER_MIN_MS: u64 = 20;
const JITTER_MAX_MS: u64 = 150;
/// Run `wal_checkpoint(PASSIVE)` once every this many committed writes.
const CHECKPOINT_INTERVAL: u64 = 50;
static PART_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
static FORK_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Journal mode the connection ended up using after [`SqliteSessionStore::open`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalMode {
    /// Write-ahead logging — the default for local filesystems.
    Wal,
    /// Rollback journal — the fallback used when WAL locking is unavailable
    /// (NFS, SMB, some FUSE mounts).
    Delete,
}

impl JournalMode {
    /// The value passed to `PRAGMA journal_mode = …`.
    fn pragma_value(self) -> &'static str {
        match self {
            Self::Wal => "WAL",
            Self::Delete => "DELETE",
        }
    }
}

/// A SQLite-backed session store.
///
/// The connection is wrapped in a [`Mutex`] so the store can be shared across
/// tasks; SQLite is a single-writer engine, so serialising writes in-process is
/// both correct and cheap.
#[derive(Debug)]
pub struct SqliteSessionStore {
    conn: Mutex<Connection>,
    data_dir: PathBuf,
    journal_mode: JournalMode,
    writes: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Media,
    ToolInput,
    ToolOutput,
    Snapshot,
    ProviderEnvelope,
    Other,
}

impl ArtifactKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::ToolInput => "tool_input",
            Self::ToolOutput => "tool_output",
            Self::Snapshot => "snapshot",
            Self::ProviderEnvelope => "provider_envelope",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArtifact {
    pub session_id: SessionId,
    pub part_id: Option<PartId>,
    pub kind: ArtifactKind,
    pub mime: String,
    pub created_at: i64,
}

#[derive(Debug)]
pub struct Artifact {
    pub row: ArtifactRow,
    pub reader: File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderPayloadDirection {
    Request,
    Response,
    StreamBatch,
    Error,
}

impl ProviderPayloadDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
            Self::StreamBatch => "stream_batch",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeStatus {
    Decoded,
    DecodedWithUnknowns,
    Failed,
    Ignored,
}

impl DecodeStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Decoded => "decoded",
            Self::DecodedWithUnknowns => "decoded_with_unknowns",
            Self::Failed => "failed",
            Self::Ignored => "ignored",
        }
    }
}

fn initial_decode_status(direction: ProviderPayloadDirection) -> &'static str {
    match direction {
        ProviderPayloadDirection::Request => "ignored",
        ProviderPayloadDirection::Response
        | ProviderPayloadDirection::StreamBatch
        | ProviderPayloadDirection::Error => "pending",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProviderPayload {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub direction: ProviderPayloadDirection,
    pub api_kind: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub sequence: u32,
    pub provider_payload_id: Option<String>,
    pub mime: String,
    pub raw_bytes: Vec<u8>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSession {
    pub title: Option<String>,
    pub source: String,
    pub workspace_root: Option<String>,
    pub system_prompt: Option<String>,
    pub settings_json: String,
    pub parent_id: Option<SessionId>,
    pub version: String,
    pub slug: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSettings {
    pub settings_json: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn is_startable(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRun {
    pub id: RunId,
    pub session_id: SessionId,
    pub status: RunStatus,
    pub trigger: Option<String>,
    pub started_at: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenDelta {
    pub input: i64,
    pub output: i64,
    pub reasoning: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

impl TokenDelta {
    fn negated(self) -> Self {
        Self {
            input: -self.input,
            output: -self.output,
            reasoning: -self.reasoning,
            cache_read: -self.cache_read,
            cache_write: -self.cache_write,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderState {
    pub run_id: RunId,
    pub api_kind: String,
    pub state_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevertInfo {
    pub message_id: MessageId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_id: Option<PartId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredPart {
    pub id: PartId,
    pub part: Part,
    pub provider_payload_id: Option<ProviderPayloadId>,
    pub provider_json_pointer: Option<String>,
    pub compacted_at: Option<i64>,
    pub created_at: i64,
}

pub type StoredTurn = (Turn, Vec<StoredPart>);

/// A row from the `turn_parts_text` projection table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnPartTextRow {
    pub part_id: PartId,
    pub turn_id: MessageId,
    pub part_type: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnPage {
    pub items: Vec<StoredTurn>,
    pub more: bool,
    pub cursor: Option<StorageCursor>,
}

impl SqliteSessionStore {
    /// Open (or create) a SQLite database at `path` and apply the standard
    /// pragmas (`journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`,
    /// `busy_timeout=5000`, `cache_size=-64000`).
    ///
    /// # Errors
    ///
    /// Returns [`SqliteStoreError`] if the database cannot be opened or a
    /// pragma cannot be applied.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        Self::open_inner(path.as_ref(), false)
    }

    /// Test seam: behave as if the `journal_mode = WAL` pragma failed with a
    /// network-filesystem locking-protocol error, exercising the DELETE
    /// fallback path that real NFS/SMB mounts trigger but tests cannot.
    #[cfg(test)]
    fn open_simulating_wal_failure(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        Self::open_inner(path.as_ref(), true)
    }

    fn open_inner(path: &Path, simulate_wal_failure: bool) -> Result<Self, SqliteStoreError> {
        let conn =
            Connection::open(path).map_err(|err| SqliteStoreError::OpenFailed(err.to_string()))?;
        apply_base_pragmas(&conn)?;
        let journal_mode = establish_journal_mode(&conn, simulate_wal_failure)?;
        migrate::migrate(&conn)
            .map_err(|err| SqliteStoreError::MigrationFailed(err.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
            data_dir: data_dir_for(path),
            journal_mode,
            writes: AtomicU64::new(0),
        })
    }

    /// The journal mode the connection is actually using.
    pub fn journal_mode(&self) -> JournalMode {
        self.journal_mode
    }

    /// Number of writes that have committed through [`Self::execute_write`].
    pub fn write_count(&self) -> u64 {
        self.writes.load(Ordering::Relaxed)
    }

    pub fn put_artifact(
        &self,
        artifact: NewArtifact,
        bytes: &[u8],
    ) -> Result<ArtifactId, SqliteStoreError> {
        let sha256 = sha256_hex(bytes);
        let relative_path = artifact_relative_path(&sha256);
        self.write_blob_if_missing(&relative_path, &sha256, bytes)?;

        let id = new_artifact_id();
        self.execute_write(|tx| {
            insert_artifact_row(tx, &id, &artifact, &sha256, &relative_path, bytes.len())
        })
    }

    pub fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact, SqliteStoreError> {
        let row = self.read_artifact_row(id)?;
        let path = self.data_dir.join(&row.path);
        if !blob_matches(&path, &row.sha256, row.size_bytes)? {
            return Err(SqliteStoreError::BlobReadFailed(format!(
                "artifact blob {} at {} does not match stored sha256",
                id,
                path.display()
            )));
        }

        let reader = File::open(&path).map_err(|err| {
            SqliteStoreError::BlobReadFailed(format!(
                "missing or unreadable artifact blob {} at {}: {err}",
                id,
                path.display()
            ))
        })?;
        Ok(Artifact { row, reader })
    }

    pub fn append_provider_payload(
        &self,
        payload: NewProviderPayload,
    ) -> Result<ProviderPayloadId, SqliteStoreError> {
        let sha256 = sha256_hex(&payload.raw_bytes);
        let relative_path = artifact_relative_path(&sha256);
        self.write_blob_if_missing(&relative_path, &sha256, &payload.raw_bytes)?;

        let artifact = NewArtifact {
            session_id: payload.session_id.clone(),
            part_id: None,
            kind: ArtifactKind::ProviderEnvelope,
            mime: payload.mime.clone(),
            created_at: payload.created_at,
        };

        let artifact_id = new_artifact_id();
        let id = new_provider_payload_id();
        self.execute_write(|tx| {
            let stored_artifact_id = insert_artifact_row(
                tx,
                &artifact_id,
                &artifact,
                &sha256,
                &relative_path,
                payload.raw_bytes.len(),
            )?;
            tx.execute(
                r#"
                INSERT INTO provider_payloads (
                    id,
                    session_id,
                    run_id,
                    direction,
                    api_kind,
                    provider_id,
                    model_id,
                    sequence,
                    provider_payload_id,
                    artifact_id,
                    sha256,
                    decode_status,
                    created_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                "#,
                params![
                    id.as_str(),
                    payload.session_id.as_str(),
                    payload.run_id.as_str(),
                    payload.direction.as_str(),
                    payload.api_kind.as_str(),
                    payload.provider_id.as_deref(),
                    payload.model_id.as_deref(),
                    i64::from(payload.sequence),
                    payload.provider_payload_id.as_deref(),
                    stored_artifact_id.as_str(),
                    sha256.as_str(),
                    initial_decode_status(payload.direction),
                    payload.created_at,
                ],
            )
        })?;
        Ok(id)
    }

    pub fn get_provider_payload(
        &self,
        id: &ProviderPayloadId,
    ) -> Result<ProviderPayloadRow, SqliteStoreError> {
        self.conn
            .lock()
            .expect("connection mutex poisoned")
            .query_row(
                r#"
                SELECT
                    id,
                    session_id,
                    run_id,
                    direction,
                    api_kind,
                    provider_id,
                    model_id,
                    sequence,
                    provider_payload_id,
                    artifact_id,
                    sha256,
                    decoder_version,
                    decode_status,
                    error_json,
                    created_at,
                    decoded_at
                FROM provider_payloads
                WHERE id = ?1
                "#,
                [id.as_str()],
                read_provider_payload_row,
            )
            .map_err(|err| read_err(err, "provider payload", id.as_str()))
    }

    pub fn list_pending_provider_payloads(
        &self,
    ) -> Result<Vec<ProviderPayloadRow>, SqliteStoreError> {
        let conn = self.conn.lock().expect("connection mutex poisoned");
        let mut statement = conn
            .prepare(
                r#"
                SELECT
                    id,
                    session_id,
                    run_id,
                    direction,
                    api_kind,
                    provider_id,
                    model_id,
                    sequence,
                    provider_payload_id,
                    artifact_id,
                    sha256,
                    decoder_version,
                    decode_status,
                    error_json,
                    created_at,
                    decoded_at
                FROM provider_payloads
                WHERE decode_status = 'pending'
                ORDER BY created_at ASC, id ASC
                "#,
            )
            .map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?;
        let rows = statement
            .query_map([], read_provider_payload_row)
            .map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?;

        let mut payloads = Vec::new();
        for row in rows {
            payloads.push(row.map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?);
        }
        Ok(payloads)
    }

    pub fn list_provider_payloads_for_run(
        &self,
        run_id: &RunId,
    ) -> Result<Vec<ProviderPayloadRow>, SqliteStoreError> {
        let conn = self.conn.lock().expect("connection mutex poisoned");
        let mut statement = conn
            .prepare(
                r#"
                SELECT
                    id,
                    session_id,
                    run_id,
                    direction,
                    api_kind,
                    provider_id,
                    model_id,
                    sequence,
                    provider_payload_id,
                    artifact_id,
                    sha256,
                    decoder_version,
                    decode_status,
                    error_json,
                    created_at,
                    decoded_at
                FROM provider_payloads
                WHERE run_id = ?1
                ORDER BY sequence ASC, created_at ASC, id ASC
                "#,
            )
            .map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?;
        let rows = statement
            .query_map([run_id.as_str()], read_provider_payload_row)
            .map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?;

        let mut payloads = Vec::new();
        for row in rows {
            payloads.push(row.map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?);
        }
        Ok(payloads)
    }

    pub fn list_decoded_provider_payloads(
        &self,
    ) -> Result<Vec<ProviderPayloadRow>, SqliteStoreError> {
        let conn = self.conn.lock().expect("connection mutex poisoned");
        let mut statement = conn
            .prepare(
                r#"
                SELECT
                    id,
                    session_id,
                    run_id,
                    direction,
                    api_kind,
                    provider_id,
                    model_id,
                    sequence,
                    provider_payload_id,
                    artifact_id,
                    sha256,
                    decoder_version,
                    decode_status,
                    error_json,
                    created_at,
                    decoded_at
                FROM provider_payloads
                WHERE decode_status IN ('decoded', 'decoded_with_unknowns')
                ORDER BY created_at ASC, id ASC
                "#,
            )
            .map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?;
        let rows = statement
            .query_map([], read_provider_payload_row)
            .map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?;

        let mut payloads = Vec::new();
        for row in rows {
            payloads.push(row.map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?);
        }
        Ok(payloads)
    }

    pub fn list_parts_for_provider_payload(
        &self,
        id: &ProviderPayloadId,
    ) -> Result<Vec<StoredPart>, SqliteStoreError> {
        let conn = self.conn.lock().expect("connection mutex poisoned");
        let mut statement = conn
            .prepare(
                r#"
                SELECT
                    turn_parts.id,
                    turn_parts.data_json,
                    turn_parts.provider_payload_id,
                    turn_parts.provider_json_pointer,
                    turn_parts.compacted_at,
                    turn_parts.created_at
                FROM turn_parts
                JOIN turns ON turns.id = turn_parts.turn_id
                WHERE turn_parts.provider_payload_id = ?1
                ORDER BY turns.seq ASC, turn_parts.created_at ASC, turn_parts.id ASC
                "#,
            )
            .map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?;
        let rows = statement
            .query_map([id.as_str()], read_stored_part)
            .map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?;

        let mut parts = Vec::new();
        for row in rows {
            parts.push(row.map_err(|err| SqliteStoreError::ReadFailed(err.to_string()))?);
        }
        Ok(parts)
    }

    pub fn mark_provider_payload_decoded(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        status: DecodeStatus,
    ) -> Result<(), SqliteStoreError> {
        let changed = self.execute_write(|tx| {
            tx.execute(
                r#"
                UPDATE provider_payloads
                SET decoder_version = ?1,
                    decode_status = ?2,
                    error_json = NULL,
                    decoded_at = ?3
                WHERE id = ?4
                "#,
                params![decoder_version, status.as_str(), unix_millis(), id.as_str(),],
            )
        })?;
        ensure_row_changed(changed, "provider payload", id.as_str())
    }

    pub fn mark_provider_payload_failed(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        error_json: &str,
    ) -> Result<(), SqliteStoreError> {
        let changed = self.execute_write(|tx| {
            tx.execute(
                r#"
                UPDATE provider_payloads
                SET decoder_version = ?1,
                    decode_status = 'failed',
                    error_json = ?2,
                    decoded_at = ?3
                WHERE id = ?4
                "#,
                params![decoder_version, error_json, unix_millis(), id.as_str(),],
            )
        })?;
        ensure_row_changed(changed, "provider payload", id.as_str())
    }

    pub fn mark_provider_payload_ignored(
        &self,
        id: &ProviderPayloadId,
        reason_json: &str,
    ) -> Result<(), SqliteStoreError> {
        let changed = self.execute_write(|tx| {
            tx.execute(
                r#"
                UPDATE provider_payloads
                SET decoder_version = NULL,
                    decode_status = 'ignored',
                    error_json = ?1,
                    decoded_at = ?2
                WHERE id = ?3
                "#,
                params![reason_json, unix_millis(), id.as_str(),],
            )
        })?;
        ensure_row_changed(changed, "provider payload", id.as_str())
    }

    pub fn append_decoded_provider_payload(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        decoded: &DecodedProviderPayload,
    ) -> Result<(), SqliteStoreError> {
        self.append_decoded_provider_payload_with_provider_state(id, decoder_version, decoded, None)
    }

    pub fn append_decoded_provider_payload_with_provider_state(
        &self,
        id: &ProviderPayloadId,
        decoder_version: &str,
        decoded: &DecodedProviderPayload,
        provider_state: Option<&ProviderState>,
    ) -> Result<(), SqliteStoreError> {
        ensure_decoded_payload_references_id(id, decoded)?;
        let prepared_turns = decoded
            .turns
            .iter()
            .map(prepare_decoded_turn)
            .collect::<Result<Vec<_>, _>>()?;
        let cost_delta = cost_delta_from_decoded(decoded)?;

        self.execute_write(|tx| {
            let payload_context = provider_payload_context(tx, id)?;
            if let Some(provider_state) = provider_state {
                ensure_provider_state_matches_payload(provider_state, &payload_context.run_id)?;
            }
            ensure_decoded_turns_match_payload(&prepared_turns, &payload_context.run_id)?;
            for turn in &prepared_turns {
                insert_turn(tx, turn)?;
            }
            let changed = tx.execute(
                r#"
                UPDATE provider_payloads
                SET decoder_version = ?1,
                    decode_status = ?2,
                    error_json = NULL,
                    decoded_at = ?3
                WHERE id = ?4
                "#,
                params![
                    decoder_version,
                    decoded.status.as_str(),
                    unix_millis(),
                    id.as_str(),
                ],
            )?;
            if changed == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            if let Some(provider_state) = provider_state {
                set_provider_state_in_tx(tx, provider_state)?;
            }
            if cost_delta.has_value() {
                update_session_cost_in_tx(tx, &payload_context.session_id, cost_delta)?;
            }
            Ok(())
        })?;
        Ok(())
    }

    pub fn get_provider_state(
        &self,
        run_id: &RunId,
    ) -> Result<Option<ProviderState>, SqliteStoreError> {
        self.conn
            .lock()
            .expect("connection mutex poisoned")
            .query_row(
                r#"
                SELECT run_id, api_kind, state_json
                FROM provider_state
                WHERE run_id = ?1
                "#,
                [run_id.as_str()],
                read_provider_state,
            )
            .optional()
            .map_err(|err| read_err(err, "provider_state", run_id.as_str()))
    }

    pub fn set_provider_state(&self, state: ProviderState) -> Result<(), SqliteStoreError> {
        self.execute_write(|tx| set_provider_state_in_tx(tx, &state))?;
        Ok(())
    }

    pub fn create_session(
        &self,
        session_id: SessionId,
        session: CreateSession,
    ) -> Result<(), SqliteStoreError> {
        self.execute_write(|tx| {
            tx.execute(
                r#"
                INSERT INTO sessions (
                    id,
                    title,
                    source,
                    workspace_root,
                    system_prompt,
                    settings_json,
                    parent_id,
                    version,
                    slug,
                    created_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
                "#,
                params![
                    session_id.as_str(),
                    session.title.as_deref(),
                    session.source.as_str(),
                    session.workspace_root.as_deref(),
                    session.system_prompt.as_deref(),
                    session.settings_json.as_str(),
                    session.parent_id.as_ref().map(SessionId::as_str),
                    session.version.as_str(),
                    session.slug.as_deref(),
                    session.created_at,
                ],
            )
        })?;
        Ok(())
    }

    pub fn get_session(&self, session_id: &SessionId) -> Result<SessionRow, SqliteStoreError> {
        let conn = self.conn.lock().expect("connection mutex poisoned");
        read_session_row_by_id(&conn, session_id)?.ok_or_else(|| SqliteStoreError::NotFound {
            entity: "session",
            id: session_id.to_string(),
        })
    }

    pub fn update_session_settings(
        &self,
        session_id: &SessionId,
        settings: SessionSettings,
    ) -> Result<(), SqliteStoreError> {
        let changed = self.execute_write(|tx| {
            tx.execute(
                "UPDATE sessions SET settings_json = ?1, updated_at = ?2 WHERE id = ?3",
                params![
                    settings.settings_json.as_str(),
                    settings.updated_at,
                    session_id.as_str()
                ],
            )
        })?;
        ensure_row_changed(changed, "session", session_id.as_str())
    }

    pub fn update_session_title(
        &self,
        session_id: &SessionId,
        title: &str,
    ) -> Result<(), SqliteStoreError> {
        let now = unix_millis();
        let changed = self.execute_write(|tx| {
            tx.execute(
                "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![title, now, session_id.as_str()],
            )
        })?;
        ensure_row_changed(changed, "session", session_id.as_str())
    }

    pub fn update_session_revert(
        &self,
        session_id: &SessionId,
        revert: &RevertInfo,
    ) -> Result<(), SqliteStoreError> {
        let revert_json = serialize_json(revert)?;
        let changed = self.execute_write(|tx| {
            tx.execute(
                "UPDATE sessions SET revert_json = ?1, updated_at = ?2 WHERE id = ?3",
                params![revert_json.as_str(), unix_millis(), session_id.as_str()],
            )
        })?;
        ensure_row_changed(changed, "session", session_id.as_str())
    }

    pub fn clear_session_revert(&self, session_id: &SessionId) -> Result<(), SqliteStoreError> {
        let changed = self.execute_write(|tx| {
            tx.execute(
                "UPDATE sessions SET revert_json = NULL, updated_at = ?1 WHERE id = ?2",
                params![unix_millis(), session_id.as_str()],
            )
        })?;
        ensure_row_changed(changed, "session", session_id.as_str())
    }

    pub fn update_session_cost(
        &self,
        session_id: &SessionId,
        delta_cost: f64,
        delta_tokens: TokenDelta,
    ) -> Result<(), SqliteStoreError> {
        let changed = self.execute_write(|tx| {
            tx.execute(
                r#"
                UPDATE sessions
                SET cost = cost + ?1,
                    tokens_input = tokens_input + ?2,
                    tokens_output = tokens_output + ?3,
                    tokens_reasoning = tokens_reasoning + ?4,
                    tokens_cache_read = tokens_cache_read + ?5,
                    tokens_cache_write = tokens_cache_write + ?6
                WHERE id = ?7
                "#,
                params![
                    delta_cost,
                    delta_tokens.input,
                    delta_tokens.output,
                    delta_tokens.reasoning,
                    delta_tokens.cache_read,
                    delta_tokens.cache_write,
                    session_id.as_str(),
                ],
            )
        })?;
        ensure_row_changed(changed, "session", session_id.as_str())
    }

    pub fn reverse_session_cost(
        &self,
        session_id: &SessionId,
        cost: f64,
        tokens: TokenDelta,
    ) -> Result<(), SqliteStoreError> {
        self.update_session_cost(session_id, -cost, tokens.negated())
    }

    pub fn start_run(&self, run: StartRun) -> Result<(), SqliteStoreError> {
        if !run.status.is_startable() {
            return Err(invalid_run_status("start_run", run.status));
        }

        self.execute_write(|tx| insert_run(tx, &run))?;
        Ok(())
    }

    pub fn get_run(&self, run_id: &RunId) -> Result<RunRow, SqliteStoreError> {
        let conn = self.conn.lock().expect("connection mutex poisoned");
        read_run_row_by_id(&conn, run_id)?.ok_or_else(|| SqliteStoreError::NotFound {
            entity: "run",
            id: run_id.to_string(),
        })
    }

    pub fn finish_run(
        &self,
        run_id: &RunId,
        status: RunStatus,
        finished_at: i64,
        error_json: Option<String>,
    ) -> Result<(), SqliteStoreError> {
        if !status.is_terminal() {
            return Err(invalid_run_status("finish_run", status));
        }

        let changed = self.execute_write(|tx| {
            finish_run_in_tx(tx, run_id, status, finished_at, error_json.as_deref())
        })?;
        if changed > 0 {
            return Ok(());
        }

        match self.get_run(run_id) {
            Ok(run) if is_terminal_status(&run.status) => {
                Err(SqliteStoreError::InvalidRunTransition {
                    id: run_id.to_string(),
                    status: run.status,
                })
            }
            Ok(_) => Err(SqliteStoreError::WriteFailed(format!(
                "run `{run_id}` did not transition to {}",
                status.as_str()
            ))),
            Err(err) => Err(err),
        }
    }

    /// Fork `source` into a brand-new session, copying its turns (and their
    /// parts) up to and including `through_message`. When `through_message` is
    /// `None` the whole transcript is copied.
    ///
    /// The fork is a fresh replay ledger: every session, run, turn, and part
    /// gets a new id, the session's `parent_id` chains back to `source` so the
    /// lineage is walkable, and the source's settings (model selection) are
    /// copied while cost/token aggregates reset to zero. `TurnMeta.parent_id`
    /// references are remapped to the new message ids so the message chain
    /// stays internally consistent inside the fork.
    ///
    /// Parts are copied by value: their provider-payload linkage
    /// (`provider_payload_id`/`provider_json_pointer`) and compaction state are
    /// intentionally dropped, because the `provider_payloads` rows belong to the
    /// source session and are not cloned. The fork is a clean canonical ledger,
    /// not a replica of the source's provider journal.
    pub fn fork_session(
        &self,
        source: &SessionId,
        through_message: Option<&MessageId>,
    ) -> Result<SessionRow, SqliteStoreError> {
        // Stamp every minted id with one timestamp so they sort in allocation
        // order. Turns that share a `created_at` are otherwise tie-broken by id
        // in `list_turns_for_session`, so minting in chronological copy order
        // keeps the forked transcript in the same order as the source.
        let now = current_time_millis();

        // `validation` carries a typed error out of the write closure: the
        // closure can only return `rusqlite::Error`, which `execute_write`
        // flattens into `WriteFailed`, so domain failures (missing session,
        // missing message) are stashed here and restored after the call.
        let mut validation: Option<SqliteStoreError> = None;
        let forked = self.execute_write(|tx| {
            validation = None; // execute_write may retry on a busy database.

            // Read the whole source snapshot inside the write transaction so
            // the fork is point-in-time consistent: BEGIN IMMEDIATE holds the
            // write lock, so no concurrent writer can change a run or part
            // between reading the transcript and copying it.
            let source_row = match read_session_row_by_id(tx, source) {
                Ok(Some(row)) => row,
                Ok(None) => {
                    return fork_abort(
                        &mut validation,
                        SqliteStoreError::NotFound {
                            entity: "session",
                            id: source.to_string(),
                        },
                    );
                }
                Err(err) => return fork_abort(&mut validation, err),
            };
            let copied = match read_turns_for_fork(tx, source, through_message) {
                Ok(turns) => turns,
                Err(err) => return fork_abort(&mut validation, err),
            };

            // Map source ids to freshly minted ids, preserving run order of
            // first appearance so the copied runs keep their original sequencing.
            let new_session_id = SessionId::new_unchecked(mint_uuid_v7(now));
            let mut new_run_ids: HashMap<RunId, RunId> = HashMap::new();
            let mut run_order: Vec<RunId> = Vec::new();
            let mut new_message_ids: HashMap<MessageId, MessageId> = HashMap::new();
            for (turn, _) in &copied {
                if !new_run_ids.contains_key(&turn.run_id) {
                    new_run_ids
                        .insert(turn.run_id.clone(), RunId::new_unchecked(mint_uuid_v7(now)));
                    run_order.push(turn.run_id.clone());
                }
                new_message_ids
                    .entry(turn.id.clone())
                    .or_insert_with(|| MessageId::new_unchecked(mint_uuid_v7(now)));
            }

            tx.execute(
                r#"
                INSERT INTO sessions (
                    id,
                    title,
                    source,
                    workspace_root,
                    system_prompt,
                    settings_json,
                    parent_id,
                    version,
                    slug,
                    created_at,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?9)
                "#,
                params![
                    new_session_id.as_str(),
                    source_row.title.as_deref(),
                    source_row.source.as_str(),
                    source_row.workspace_root.as_deref(),
                    source_row.system_prompt.as_deref(),
                    source_row.settings_json.as_str(),
                    source.as_str(),
                    source_row.version.as_str(),
                    now as i64,
                ],
            )?;

            for source_run_id in &run_order {
                let run = match read_run_row_by_id(tx, source_run_id) {
                    Ok(Some(run)) => run,
                    Ok(None) => {
                        return fork_abort(
                            &mut validation,
                            SqliteStoreError::NotFound {
                                entity: "run",
                                id: source_run_id.to_string(),
                            },
                        );
                    }
                    Err(err) => return fork_abort(&mut validation, err),
                };
                tx.execute(
                    r#"
                    INSERT INTO runs (
                        id,
                        session_id,
                        status,
                        trigger,
                        started_at,
                        finished_at,
                        error_json
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    "#,
                    params![
                        new_run_ids[source_run_id].as_str(),
                        new_session_id.as_str(),
                        run.status.as_str(),
                        run.trigger.as_deref(),
                        run.started_at,
                        run.finished_at,
                        run.error_json.as_deref(),
                    ],
                )?;
            }

            for (turn, parts) in &copied {
                let mut forked = turn.clone();
                forked.id = new_message_ids[&turn.id].clone();
                forked.run_id = new_run_ids[&turn.run_id].clone();
                forked.meta.parent_id = turn
                    .meta
                    .parent_id
                    .as_ref()
                    .and_then(|parent| new_message_ids.get(parent).cloned());
                let part_values = parts
                    .iter()
                    .map(|stored| stored.part.clone())
                    .collect::<Vec<_>>();
                let prepared = match prepare_turn(&forked, &part_values) {
                    Ok(prepared) => prepared,
                    Err(err) => return fork_abort(&mut validation, err),
                };
                insert_turn(tx, &prepared)?;
            }

            Ok(new_session_id)
        });

        let new_session_id = match forked {
            Ok(id) => id,
            Err(write_err) => return Err(validation.unwrap_or(write_err)),
        };
        self.get_session(&new_session_id)
    }

    pub fn append_finished_run_with_turns(
        &self,
        run: StartRun,
        turns_with_parts: &[(Turn, Vec<Part>)],
        finished_at: i64,
        status: RunStatus,
        error_json: Option<String>,
    ) -> Result<(), SqliteStoreError> {
        if !run.status.is_startable() {
            return Err(invalid_run_status(
                "append_finished_run_with_turns",
                run.status,
            ));
        }
        if !status.is_terminal() {
            return Err(invalid_run_status("append_finished_run_with_turns", status));
        }

        let prepared_turns = turns_with_parts
            .iter()
            .map(|(turn, parts)| prepare_turn(turn, parts))
            .collect::<Result<Vec<_>, _>>()?;

        self.execute_write(|tx| {
            insert_run(tx, &run)?;
            for turn in &prepared_turns {
                insert_turn(tx, turn)?;
            }
            let changed =
                finish_run_in_tx(tx, &run.id, status, finished_at, error_json.as_deref())?;
            if changed == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            Ok(())
        })?;
        Ok(())
    }

    pub fn append_turn(&self, turn: Turn, parts: Vec<Part>) -> Result<(), SqliteStoreError> {
        self.append_turns(&[(turn, parts)])
    }

    pub fn append_turns(
        &self,
        turns_with_parts: &[(Turn, Vec<Part>)],
    ) -> Result<(), SqliteStoreError> {
        if turns_with_parts.is_empty() {
            return Ok(());
        }

        let prepared_turns = turns_with_parts
            .iter()
            .map(|(turn, parts)| prepare_turn(turn, parts))
            .collect::<Result<Vec<_>, _>>()?;

        self.execute_write(|tx| {
            for turn in &prepared_turns {
                insert_turn(tx, turn)?;
            }
            Ok(())
        })?;
        Ok(())
    }

    pub fn list_turns_for_run(&self, run_id: &RunId) -> Result<Vec<StoredTurn>, SqliteStoreError> {
        let conn = self.conn.lock().expect("connection mutex poisoned");
        let mut turn_stmt = conn
            .prepare(
                r#"
                SELECT id, run_id, seq, role, meta_json, created_at
                FROM turns
                WHERE run_id = ?1
                ORDER BY seq ASC
                "#,
            )
            .map_err(read_query_err)?;
        let turns = turn_stmt
            .query_map([run_id.as_str()], read_turn)
            .map_err(read_query_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(read_query_err)?;
        drop(turn_stmt);

        collect_parts_for_turns(&conn, turns)
    }

    pub fn list_turns_for_session(
        &self,
        session_id: &SessionId,
        cursor: Option<StorageCursor>,
        limit: usize,
    ) -> Result<TurnPage, SqliteStoreError> {
        if limit == 0 {
            return Ok(TurnPage {
                items: Vec::new(),
                more: false,
                cursor: None,
            });
        }

        let conn = self.conn.lock().expect("connection mutex poisoned");
        let query_limit = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        let mut turns = match cursor {
            Some(cursor) => {
                let mut stmt = conn
                    .prepare(
                        r#"
                        SELECT t.id, t.run_id, t.seq, t.role, t.meta_json, t.created_at
                        FROM turns t
                        JOIN runs r ON r.id = t.run_id
                        WHERE r.session_id = ?1
                          AND (
                            t.created_at < ?2
                            OR (t.created_at = ?2 AND t.id < ?3)
                          )
                        ORDER BY t.created_at DESC, t.id DESC
                        LIMIT ?4
                        "#,
                    )
                    .map_err(read_query_err)?;
                stmt.query_map(
                    params![
                        session_id.as_str(),
                        cursor.created_at,
                        cursor.id.as_str(),
                        query_limit,
                    ],
                    read_turn,
                )
                .map_err(read_query_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(read_query_err)?
            }
            None => {
                let mut stmt = conn
                    .prepare(
                        r#"
                        SELECT t.id, t.run_id, t.seq, t.role, t.meta_json, t.created_at
                        FROM turns t
                        JOIN runs r ON r.id = t.run_id
                        WHERE r.session_id = ?1
                        ORDER BY t.created_at DESC, t.id DESC
                        LIMIT ?2
                        "#,
                    )
                    .map_err(read_query_err)?;
                stmt.query_map(params![session_id.as_str(), query_limit], read_turn)
                    .map_err(read_query_err)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(read_query_err)?
            }
        };

        let more = turns.len() > limit;
        if more {
            turns.truncate(limit);
        }

        let cursor = if more {
            turns
                .last()
                .map(|turn| StorageCursor::new(turn.created_at, turn.id.to_string()))
        } else {
            None
        };
        let items = collect_parts_for_turns(&conn, turns)?;

        Ok(TurnPage {
            items,
            more,
            cursor,
        })
    }

    pub fn update_part(&self, part_id: &PartId, part: Part) -> Result<(), SqliteStoreError> {
        let type_name = part.type_name().to_string();
        let data_json = serialize_json(&part)?;
        let changed = self.execute_write(|tx| {
            let Some(existing) = read_existing_part(tx, part_id)? else {
                return Ok(0);
            };
            let changed = tx.execute(
                r#"
                UPDATE turn_parts
                SET type = ?1,
                    data_json = ?2
                WHERE id = ?3
                "#,
                params![type_name.as_str(), data_json.as_str(), part_id.as_str()],
            )?;
            if changed > 0 {
                let cost_delta = cost_delta_for_part_replacement(&existing.part, &part)?;
                if cost_delta.has_value() {
                    update_session_cost_in_tx(tx, &existing.session_id, cost_delta)?;
                }
            }
            Ok(changed)
        })?;
        ensure_row_changed(changed, "turn_part", part_id.as_str())
    }

    pub fn update_part_delta(
        &self,
        turn_id: &MessageId,
        part_id: &PartId,
        field: &str,
        delta: &str,
    ) -> Result<(), SqliteStoreError> {
        let json_path = delta_json_path_for_field(field)?;
        let changed = self.execute_write(|tx| {
            tx.execute(
                r#"
                UPDATE turn_parts
                SET data_json = json_set(
                    data_json,
                    ?1,
                    COALESCE(json_extract(data_json, ?1), '') || ?2
                )
                WHERE id = ?3
                  AND turn_id = ?4
                "#,
                params![json_path, delta, part_id.as_str(), turn_id.as_str()],
            )
        })?;
        ensure_row_changed(changed, "turn_part", part_id.as_str())
    }

    pub fn compact_part(&self, part_id: &PartId) -> Result<(), SqliteStoreError> {
        let compacted_at = i64::try_from(current_time_millis()).unwrap_or(i64::MAX);
        let changed = self.execute_write(|tx| {
            tx.execute(
                "UPDATE turn_parts SET compacted_at = ?1 WHERE id = ?2",
                params![compacted_at, part_id.as_str()],
            )
        })?;
        ensure_row_changed(changed, "turn_part", part_id.as_str())
    }

    pub fn remove_part(
        &self,
        turn_id: &MessageId,
        part_id: &PartId,
    ) -> Result<(), SqliteStoreError> {
        let changed = self.execute_write(|tx| {
            let Some(existing) = read_existing_part_for_turn(tx, turn_id, part_id)? else {
                return Ok(0);
            };
            let changed = tx.execute(
                "DELETE FROM turn_parts WHERE id = ?1 AND turn_id = ?2",
                params![part_id.as_str(), turn_id.as_str()],
            )?;
            if changed > 0 {
                let cost_delta = cost_delta_for_part_removal(&existing.part)?;
                if cost_delta.has_value() {
                    update_session_cost_in_tx(tx, &existing.session_id, cost_delta)?;
                }
            }
            Ok(changed)
        })?;
        ensure_row_changed(changed, "turn_part", part_id.as_str())
    }

    /// Query the `turn_parts_text` projection for a session.
    ///
    /// Returns one row per `turn_parts` row that has a non-empty text
    /// projection (currently `text`, `tool_result`, and `thinking` types).
    pub fn get_turn_parts_text(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<TurnPartTextRow>, SqliteStoreError> {
        let conn = self.conn.lock().expect("connection mutex poisoned");
        let mut stmt = conn
            .prepare(
                r#"
                SELECT tpt.part_id, tpt.turn_id, tpt.part_type, tpt.text
                FROM turn_parts_text tpt
                JOIN turn_parts tp ON tp.id = tpt.part_id
                WHERE tp.session_id = ?1
                ORDER BY tpt.part_id ASC
                "#,
            )
            .map_err(read_query_err)?;
        let rows = stmt
            .query_map(params![session_id.as_str()], |row| {
                Ok(TurnPartTextRow {
                    part_id: PartId::new_unchecked(row.get::<_, String>(0)?),
                    turn_id: MessageId::new_unchecked(row.get::<_, String>(1)?),
                    part_type: row.get(2)?,
                    text: row.get(3)?,
                })
            })
            .map_err(read_query_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(read_query_err)?;
        Ok(rows)
    }

    pub fn next_turn_created_at_for_run(
        &self,
        run_id: &RunId,
        now: i64,
    ) -> Result<i64, SqliteStoreError> {
        let latest: Option<i64> = self
            .conn
            .lock()
            .expect("connection mutex poisoned")
            .query_row(
                r#"
                SELECT MAX(t.created_at)
                FROM runs target
                JOIN runs session_runs ON session_runs.session_id = target.session_id
                LEFT JOIN turns t ON t.run_id = session_runs.id
                WHERE target.id = ?1
                "#,
                [run_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|err| read_err(err, "run", run_id.as_str()))?;
        Ok(latest
            .and_then(|created_at| created_at.checked_add(1))
            .unwrap_or(now)
            .max(now))
    }

    /// Run `op` inside a `BEGIN IMMEDIATE` transaction, committing on success.
    ///
    /// The immediate transaction acquires the write lock up front; on a busy
    /// database the call is retried with random 20–150ms jitter (up to 15
    /// attempts) so concurrent writers don't form a convoy. Every
    /// [`CHECKPOINT_INTERVAL`] committed writes triggers a passive WAL
    /// checkpoint to keep the WAL from growing unbounded.
    ///
    /// # Errors
    ///
    /// Returns [`SqliteStoreError::WriteFailed`] if `op` fails or the
    /// transaction cannot commit within the retry budget.
    pub fn execute_write<T, F>(&self, mut op: F) -> Result<T, SqliteStoreError>
    where
        F: FnMut(&Transaction) -> rusqlite::Result<T>,
    {
        let result = {
            let mut conn = self.conn.lock().expect("connection mutex poisoned");
            run_immediate_with_retry(&mut conn, &mut op)?
        };

        let writes = self.writes.fetch_add(1, Ordering::Relaxed) + 1;
        if self.journal_mode == JournalMode::Wal && should_checkpoint(writes) {
            self.checkpoint();
        }
        Ok(result)
    }

    /// Flush committed WAL frames back into the main database file.
    ///
    /// Best-effort: the write has already committed by the time this runs, so a
    /// checkpoint failure must not be reported as a write failure. A passive
    /// checkpoint never blocks and self-corrects on the next pass, so failures
    /// here are benign and intentionally swallowed.
    fn checkpoint(&self) {
        let _ = self
            .conn
            .lock()
            .expect("connection mutex poisoned")
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE)");
    }

    fn read_artifact_row(&self, id: &ArtifactId) -> Result<ArtifactRow, SqliteStoreError> {
        self.conn
            .lock()
            .expect("connection mutex poisoned")
            .query_row(
                "SELECT id, session_id, part_id, kind, mime, sha256, path, size_bytes, created_at
                 FROM artifacts
                 WHERE id = ?1",
                [id.as_str()],
                artifact_row_from_sql,
            )
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => {
                    SqliteStoreError::ArtifactNotFound(id.to_string())
                }
                other => SqliteStoreError::ReadFailed(other.to_string()),
            })
    }

    fn write_blob_if_missing(
        &self,
        relative_path: &str,
        sha256: &str,
        bytes: &[u8],
    ) -> Result<(), SqliteStoreError> {
        let path = self.data_dir.join(relative_path);
        if blob_matches(&path, sha256, bytes.len() as u64)? {
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| SqliteStoreError::BlobWriteFailed(err.to_string()))?;
        }

        let temp_path = temp_blob_path(&path);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(mut file) => {
                if let Err(err) = file.write_all(bytes) {
                    let _ = fs::remove_file(&temp_path);
                    return Err(SqliteStoreError::BlobWriteFailed(err.to_string()));
                }

                drop(file);
                fs::rename(&temp_path, &path).map_err(|err| {
                    let _ = fs::remove_file(&temp_path);
                    SqliteStoreError::BlobWriteFailed(err.to_string())
                })
            }
            Err(err) => Err(SqliteStoreError::BlobWriteFailed(err.to_string())),
        }
    }

    /// Read an integer-valued pragma. Test-only diagnostic helper.
    #[cfg(test)]
    fn pragma_i64(&self, name: &str) -> i64 {
        self.conn
            .lock()
            .expect("connection mutex poisoned")
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .expect("pragma query should succeed")
    }
}

struct PreparedTurn {
    turn: Turn,
    meta_json: String,
    parts: Vec<PreparedPart>,
}

struct PreparedPart {
    id: PartId,
    type_name: String,
    data_json: String,
    provider_payload_id: Option<ProviderPayloadId>,
    provider_json_pointer: Option<String>,
    created_at: i64,
}

struct ExistingPart {
    session_id: String,
    part: Part,
}

struct ProviderPayloadContext {
    session_id: String,
    run_id: RunId,
}

#[derive(Debug, Clone, Copy, Default)]
struct CostDelta {
    cost: f64,
    tokens: TokenDelta,
}

impl CostDelta {
    fn has_value(self) -> bool {
        self.cost != 0.0 || self.tokens != TokenDelta::default()
    }

    fn add_part(&mut self, part: &Part) -> Result<(), String> {
        if let Part::StepFinish { cost, tokens, .. } = part {
            self.cost += cost;
            self.add_tokens(tokens)?;
        }
        Ok(())
    }

    fn subtract_part(&mut self, part: &Part) -> Result<(), String> {
        if let Part::StepFinish { cost, tokens, .. } = part {
            self.cost -= cost;
            self.subtract_tokens(tokens)?;
        }
        Ok(())
    }

    fn add_tokens(&mut self, tokens: &TokenUsage) -> Result<(), String> {
        self.tokens.input = checked_token_add(self.tokens.input, tokens.input, "input")?;
        self.tokens.output = checked_token_add(self.tokens.output, tokens.output, "output")?;
        self.tokens.reasoning =
            checked_token_add(self.tokens.reasoning, tokens.reasoning, "reasoning")?;
        self.tokens.cache_read =
            checked_token_add(self.tokens.cache_read, tokens.cache_read, "cache_read")?;
        self.tokens.cache_write =
            checked_token_add(self.tokens.cache_write, tokens.cache_write, "cache_write")?;
        Ok(())
    }

    fn subtract_tokens(&mut self, tokens: &TokenUsage) -> Result<(), String> {
        self.tokens.input = checked_token_sub(self.tokens.input, tokens.input, "input")?;
        self.tokens.output = checked_token_sub(self.tokens.output, tokens.output, "output")?;
        self.tokens.reasoning =
            checked_token_sub(self.tokens.reasoning, tokens.reasoning, "reasoning")?;
        self.tokens.cache_read =
            checked_token_sub(self.tokens.cache_read, tokens.cache_read, "cache_read")?;
        self.tokens.cache_write =
            checked_token_sub(self.tokens.cache_write, tokens.cache_write, "cache_write")?;
        Ok(())
    }
}

fn prepare_turn(turn: &Turn, parts: &[Part]) -> Result<PreparedTurn, SqliteStoreError> {
    Ok(PreparedTurn {
        turn: turn.clone(),
        meta_json: serialize_json(&turn.meta)?,
        parts: prepare_parts(turn.created_at, parts)?,
    })
}

fn prepare_decoded_turn(decoded: &DecodedTurn) -> Result<PreparedTurn, SqliteStoreError> {
    Ok(PreparedTurn {
        turn: decoded.turn.clone(),
        meta_json: serialize_json(&decoded.turn.meta)?,
        parts: decoded
            .parts
            .iter()
            .map(|part| {
                Ok(PreparedPart {
                    id: generate_part_id(decoded.turn.created_at),
                    type_name: part.part.type_name().to_string(),
                    data_json: serialize_json(&part.part)?,
                    provider_payload_id: Some(part.provider_payload_id.clone()),
                    provider_json_pointer: Some(part.provider_json_pointer.clone()),
                    created_at: decoded.turn.created_at,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn ensure_decoded_payload_references_id(
    id: &ProviderPayloadId,
    decoded: &DecodedProviderPayload,
) -> Result<(), SqliteStoreError> {
    let all_parts_match = decoded.turns.iter().all(|turn| {
        turn.parts
            .iter()
            .all(|part| part.provider_payload_id == *id)
    });
    if all_parts_match {
        return Ok(());
    }

    Err(SqliteStoreError::WriteFailed(format!(
        "decoded parts reference a different provider payload than `{id}`"
    )))
}

fn cost_delta_from_decoded(
    decoded: &DecodedProviderPayload,
) -> Result<CostDelta, SqliteStoreError> {
    let mut delta = CostDelta::default();
    for decoded_turn in &decoded.turns {
        for decoded_part in &decoded_turn.parts {
            delta
                .add_part(&decoded_part.part)
                .map_err(SqliteStoreError::WriteFailed)?;
        }
    }
    Ok(delta)
}

fn cost_delta_for_part_replacement(old: &Part, new: &Part) -> rusqlite::Result<CostDelta> {
    let mut delta = CostDelta::default();
    delta.subtract_part(old).map_err(from_sql_error)?;
    delta.add_part(new).map_err(from_sql_error)?;
    Ok(delta)
}

fn cost_delta_for_part_removal(part: &Part) -> rusqlite::Result<CostDelta> {
    let mut delta = CostDelta::default();
    delta.subtract_part(part).map_err(from_sql_error)?;
    Ok(delta)
}

fn checked_token_add(current: i64, next: u64, field: &str) -> Result<i64, String> {
    let next = token_delta_i64(next, field)?;
    current
        .checked_add(next)
        .ok_or_else(|| format!("StepFinish {field} token total overflows i64"))
}

fn checked_token_sub(current: i64, next: u64, field: &str) -> Result<i64, String> {
    let next = token_delta_i64(next, field)?;
    current
        .checked_sub(next)
        .ok_or_else(|| format!("StepFinish {field} token total overflows i64"))
}

fn token_delta_i64(value: u64, field: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("StepFinish {field} token delta overflows i64"))
}

fn provider_payload_context(
    tx: &Transaction,
    id: &ProviderPayloadId,
) -> rusqlite::Result<ProviderPayloadContext> {
    tx.query_row(
        "SELECT session_id, run_id FROM provider_payloads WHERE id = ?1",
        [id.as_str()],
        |row| {
            let run_id: String = row.get("run_id")?;
            Ok(ProviderPayloadContext {
                session_id: row.get("session_id")?,
                run_id: RunId::new_unchecked(run_id),
            })
        },
    )
}

fn ensure_provider_state_matches_payload(
    provider_state: &ProviderState,
    payload_run_id: &RunId,
) -> rusqlite::Result<()> {
    if provider_state.run_id == *payload_run_id {
        return Ok(());
    }

    Err(from_sql_error(format!(
        "provider_state run_id `{}` does not match provider payload run `{}`",
        provider_state.run_id, payload_run_id
    )))
}

fn ensure_decoded_turns_match_payload(
    turns: &[PreparedTurn],
    payload_run_id: &RunId,
) -> rusqlite::Result<()> {
    for prepared in turns {
        if prepared.turn.run_id != *payload_run_id {
            return Err(from_sql_error(format!(
                "decoded turn run_id `{}` does not match provider payload run `{}`",
                prepared.turn.run_id, payload_run_id
            )));
        }
    }
    Ok(())
}

fn update_session_cost_in_tx(
    tx: &Transaction,
    session_id: &str,
    delta: CostDelta,
) -> rusqlite::Result<()> {
    let changed = tx.execute(
        r#"
        UPDATE sessions
        SET cost = cost + ?1,
            tokens_input = tokens_input + ?2,
            tokens_output = tokens_output + ?3,
            tokens_reasoning = tokens_reasoning + ?4,
            tokens_cache_read = tokens_cache_read + ?5,
            tokens_cache_write = tokens_cache_write + ?6
        WHERE id = ?7
        "#,
        params![
            delta.cost,
            delta.tokens.input,
            delta.tokens.output,
            delta.tokens.reasoning,
            delta.tokens.cache_read,
            delta.tokens.cache_write,
            session_id,
        ],
    )?;
    if changed == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

fn read_existing_part(
    tx: &Transaction,
    part_id: &PartId,
) -> rusqlite::Result<Option<ExistingPart>> {
    tx.query_row(
        "SELECT session_id, data_json FROM turn_parts WHERE id = ?1",
        [part_id.as_str()],
        read_existing_part_row,
    )
    .optional()
}

fn read_existing_part_for_turn(
    tx: &Transaction,
    turn_id: &MessageId,
    part_id: &PartId,
) -> rusqlite::Result<Option<ExistingPart>> {
    tx.query_row(
        "SELECT session_id, data_json FROM turn_parts WHERE id = ?1 AND turn_id = ?2",
        params![part_id.as_str(), turn_id.as_str()],
        read_existing_part_row,
    )
    .optional()
}

fn read_existing_part_row(row: &Row<'_>) -> rusqlite::Result<ExistingPart> {
    let data_json: String = row.get("data_json")?;
    Ok(ExistingPart {
        session_id: row.get("session_id")?,
        part: parse_json(&data_json)?,
    })
}

fn set_provider_state_in_tx(tx: &Transaction, state: &ProviderState) -> rusqlite::Result<()> {
    tx.execute(
        r#"
        INSERT INTO provider_state (run_id, api_kind, state_json)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(run_id) DO UPDATE SET
            api_kind = excluded.api_kind,
            state_json = excluded.state_json
        "#,
        params![
            state.run_id.as_str(),
            state.api_kind.as_str(),
            state.state_json.as_str(),
        ],
    )?;
    Ok(())
}

fn insert_run(tx: &Transaction, run: &StartRun) -> rusqlite::Result<usize> {
    tx.execute(
        r#"
        INSERT INTO runs (
            id,
            session_id,
            status,
            trigger,
            started_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        params![
            run.id.as_str(),
            run.session_id.as_str(),
            run.status.as_str(),
            run.trigger.as_deref(),
            run.started_at,
        ],
    )
}

fn finish_run_in_tx(
    tx: &Transaction,
    run_id: &RunId,
    status: RunStatus,
    finished_at: i64,
    error_json: Option<&str>,
) -> rusqlite::Result<usize> {
    tx.execute(
        r#"
        UPDATE runs
        SET status = ?1,
            finished_at = ?2,
            error_json = ?3
        WHERE id = ?4
          AND status IN ('pending', 'running')
        "#,
        params![status.as_str(), finished_at, error_json, run_id.as_str(),],
    )
}

fn insert_turn(tx: &Transaction, prepared: &PreparedTurn) -> rusqlite::Result<()> {
    let turn = &prepared.turn;
    let session_id: String = tx.query_row(
        "SELECT session_id FROM runs WHERE id = ?1",
        [turn.run_id.as_str()],
        |row| row.get(0),
    )?;
    let seq: u32 = tx.query_row(
        "SELECT COALESCE(MAX(seq), -1) + 1 FROM turns WHERE run_id = ?1",
        [turn.run_id.as_str()],
        |row| row.get(0),
    )?;

    tx.execute(
        r#"
        INSERT INTO turns (
            id,
            run_id,
            seq,
            role,
            meta_json,
            created_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
        params![
            turn.id.as_str(),
            turn.run_id.as_str(),
            seq,
            turn_role_name(turn.role),
            prepared.meta_json.as_str(),
            turn.created_at,
        ],
    )?;

    for part in &prepared.parts {
        tx.execute(
            r#"
            INSERT INTO turn_parts (
                id,
                turn_id,
                session_id,
                type,
                data_json,
                provider_payload_id,
                provider_json_pointer,
                created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                part.id.as_str(),
                turn.id.as_str(),
                session_id.as_str(),
                part.type_name.as_str(),
                part.data_json.as_str(),
                part.provider_payload_id
                    .as_ref()
                    .map(ProviderPayloadId::as_str),
                part.provider_json_pointer.as_deref(),
                part.created_at,
            ],
        )?;
    }

    Ok(())
}

fn prepare_parts(created_at: i64, parts: &[Part]) -> Result<Vec<PreparedPart>, SqliteStoreError> {
    parts
        .iter()
        .map(|part| {
            let type_name = part.type_name().to_string();
            let data_json = serialize_json(part)?;
            Ok(PreparedPart {
                id: generate_part_id(created_at),
                type_name,
                data_json,
                provider_payload_id: None,
                provider_json_pointer: None,
                created_at,
            })
        })
        .collect()
}

fn serialize_json<T>(value: &T) -> Result<String, SqliteStoreError>
where
    T: serde::Serialize,
{
    serde_json::to_string(value).map_err(|err| SqliteStoreError::WriteFailed(err.to_string()))
}

fn generate_part_id(created_at: i64) -> PartId {
    let timestamp = u64::try_from(created_at).unwrap_or(0);
    let counter = PART_ID_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let entropy = (u64::from(std::process::id()) << 32) | (counter & 0xffff_ffff);
    PartId::new_unchecked(format!("prt_{timestamp:016x}_{entropy:016x}"))
}

/// Mint a fresh UUIDv7 string for forked sessions, runs, and messages, stamped
/// with the caller-supplied `timestamp_ms`.
///
/// Ids minted with the same timestamp sort in allocation order: the process id
/// lands in the high (`rand_a`) bits so concurrent processes writing the same
/// database do not collide, and a per-process monotonic counter lands in the
/// low (`rand_b`) bits so back-to-back ids stay strictly increasing. A single
/// `fork_session` call passes one timestamp for every id it mints, so the copy
/// order is preserved when turns sharing a `created_at` are tie-broken by id.
///
/// The counter occupies the low 62 bits of `rand_b`; ordering and uniqueness
/// hold until it wraps at 2^62 (~146k years at 1M ids/sec), so do not widen its
/// role assuming 64-bit headroom. Cross-process uniqueness is best-effort (only
/// the low 12 pid bits separate processes), so concurrent forks of the same
/// database rely on the insert's PRIMARY KEY conflict rolling the transaction
/// back cleanly, not on globally unique ids.
fn mint_uuid_v7(timestamp_ms: u64) -> String {
    let timestamp = timestamp_ms & 0xffff_ffff_ffff;
    let counter = FORK_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let rand_a = (u64::from(std::process::id()) & 0x0fff) as u16;
    let rand_b_high = ((counter >> 48) & 0x3fff) as u16;
    let rand_b_low = counter & 0xffff_ffff_ffff;
    format!(
        "{:08x}-{:04x}-7{:03x}-{:04x}-{:012x}",
        (timestamp >> 16) as u32,
        (timestamp & 0xffff) as u16,
        rand_a,
        0x8000 | rand_b_high,
        rand_b_low
    )
}

fn current_time_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn delta_json_path_for_field(field: &str) -> Result<&'static str, SqliteStoreError> {
    match field {
        "text" => Ok("$.text"),
        "content" => Ok("$.content"),
        other => Err(SqliteStoreError::WriteFailed(format!(
            "cannot append delta to non-streaming JSON field `{other}`"
        ))),
    }
}

fn turn_role_name(role: TurnRole) -> &'static str {
    match role {
        TurnRole::User => "user",
        TurnRole::Assistant => "assistant",
    }
}

fn collect_parts_for_turns(
    conn: &Connection,
    turns: Vec<Turn>,
) -> Result<Vec<StoredTurn>, SqliteStoreError> {
    let mut part_stmt = conn
        .prepare(
            r#"
            SELECT id, data_json, provider_payload_id, provider_json_pointer, compacted_at, created_at
            FROM turn_parts
            WHERE turn_id = ?1
            ORDER BY id ASC
            "#,
        )
        .map_err(read_query_err)?;

    turns
        .into_iter()
        .map(|turn| {
            let parts = part_stmt
                .query_map([turn.id.as_str()], read_stored_part)
                .map_err(read_query_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(read_query_err)?;
            Ok((turn, parts))
        })
        .collect()
}

/// Apply the durability/concurrency pragmas that do not vary by filesystem.
fn apply_base_pragmas(conn: &Connection) -> Result<(), SqliteStoreError> {
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(pragma_err)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(pragma_err)?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(pragma_err)?;
    conn.pragma_update(None, "cache_size", -64000)
        .map_err(pragma_err)?;
    Ok(())
}

/// Run an immediate transaction, retrying with jittered backoff while the
/// database is busy. Non-busy failures abort immediately.
fn run_immediate_with_retry<T, F>(conn: &mut Connection, op: &mut F) -> Result<T, SqliteStoreError>
where
    F: FnMut(&Transaction) -> rusqlite::Result<T>,
{
    let mut attempt: u32 = 0;
    loop {
        match try_immediate(conn, op) {
            Ok(value) => return Ok(value),
            Err(err) if is_busy(&err) && attempt < MAX_WRITE_RETRIES => {
                attempt += 1;
                std::thread::sleep(jitter_delay(attempt));
            }
            Err(err) => return Err(SqliteStoreError::WriteFailed(err.to_string())),
        }
    }
}

/// Whether a passive WAL checkpoint is due after `write_count` committed writes.
fn should_checkpoint(write_count: u64) -> bool {
    write_count != 0 && write_count.is_multiple_of(CHECKPOINT_INTERVAL)
}

/// One attempt: open `BEGIN IMMEDIATE`, run `op`, commit.
fn try_immediate<T, F>(conn: &mut Connection, op: &mut F) -> rusqlite::Result<T>
where
    F: FnMut(&Transaction) -> rusqlite::Result<T>,
{
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let value = op(&tx)?;
    tx.commit()?;
    Ok(value)
}

/// True when SQLite reports the database as busy or locked.
fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.code == rusqlite::ErrorCode::DatabaseBusy
                || e.code == rusqlite::ErrorCode::DatabaseLocked
    )
}

/// A pseudo-random backoff in `[JITTER_MIN_MS, JITTER_MAX_MS]`. Mixing the
/// sub-second clock with the attempt number keeps concurrent writers from
/// retrying in lockstep — no RNG dependency required, and the value only needs
/// to land in range.
fn jitter_delay(attempt: u32) -> Duration {
    let span = JITTER_MAX_MS - JITTER_MIN_MS + 1;
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::from(elapsed.subsec_nanos()))
        .unwrap_or(0);
    let offset = now_nanos.wrapping_add(u64::from(attempt)) % span;
    Duration::from_millis(JITTER_MIN_MS + offset)
}

/// Establish the journal mode, preferring WAL and falling back to DELETE when
/// the filesystem rejects WAL with a locking-protocol error (NFS, SMB, FUSE).
fn establish_journal_mode(
    conn: &Connection,
    simulate_wal_failure: bool,
) -> Result<JournalMode, SqliteStoreError> {
    if !simulate_wal_failure {
        match set_journal_mode(conn, JournalMode::Wal) {
            Ok(mode) => return Ok(mode),
            // Non-WAL filesystem errors are fatal; only locking-protocol
            // failures fall through to the rollback-journal fallback.
            Err(SqliteStoreError::Locking(_)) => {}
            Err(other) => return Err(other),
        }
    }

    warn_wal_fallback_once();
    set_journal_mode(conn, JournalMode::Delete)
}

/// Emit a single WARNING per process when WAL is unavailable.
fn warn_wal_fallback_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "nav: SQLite WAL journal mode unavailable on this filesystem \
             (locking protocol); falling back to DELETE journal mode"
        );
    });
}

/// True when `err` is the SQLite locking-protocol failure (`SQLITE_PROTOCOL`)
/// raised on network filesystems that cannot support WAL's shared-memory
/// locking. Matched on the structured error code rather than message text so it
/// stays robust across SQLite versions and locales.
fn is_locking_protocol_error(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if e.code == rusqlite::ErrorCode::FileLockingProtocolFailed
    )
}

/// Request a journal mode and return the mode SQLite actually applied.
fn set_journal_mode(
    conn: &Connection,
    requested: JournalMode,
) -> Result<JournalMode, SqliteStoreError> {
    let applied: String = conn
        .query_row(
            &format!("PRAGMA journal_mode = {}", requested.pragma_value()),
            [],
            |row| row.get(0),
        )
        .map_err(|err| {
            if is_locking_protocol_error(&err) {
                SqliteStoreError::Locking(err.to_string())
            } else {
                pragma_err(err)
            }
        })?;

    match applied.to_ascii_lowercase().as_str() {
        "wal" => Ok(JournalMode::Wal),
        "delete" => Ok(JournalMode::Delete),
        other => Err(SqliteStoreError::PragmaFailed(format!(
            "unexpected journal_mode '{other}' after requesting {}",
            requested.pragma_value()
        ))),
    }
}

fn pragma_err(err: rusqlite::Error) -> SqliteStoreError {
    SqliteStoreError::PragmaFailed(err.to_string())
}

fn data_dir_for(db_path: &Path) -> PathBuf {
    let data_dir = db_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    data_dir
        .canonicalize()
        .unwrap_or_else(|_| data_dir.to_path_buf())
}

fn artifact_relative_path(sha256: &str) -> String {
    format!("blobs/{}/{}", &sha256[..2], sha256)
}

fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

fn digest_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn blob_matches(path: &Path, sha256: &str, size_bytes: u64) -> Result<bool, SqliteStoreError> {
    let Ok(file) = File::open(path) else {
        return Ok(false);
    };

    let metadata = file
        .metadata()
        .map_err(|err| SqliteStoreError::BlobReadFailed(err.to_string()))?;
    if metadata.len() != size_bytes {
        return Ok(false);
    }

    let digest = file_sha256_hex(file)?;
    Ok(digest == sha256)
}

fn file_sha256_hex(mut file: File) -> Result<String, SqliteStoreError> {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = file
            .read(&mut buffer)
            .map_err(|err| SqliteStoreError::BlobReadFailed(err.to_string()))?;
        if bytes_read == 0 {
            break;
        }
        context.update(&buffer[..bytes_read]);
    }
    Ok(digest_hex(context.finish().as_ref()))
}

fn temp_blob_path(path: &Path) -> PathBuf {
    static NEXT_TEMP_BLOB: AtomicU64 = AtomicU64::new(0);

    let counter = NEXT_TEMP_BLOB.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("blob");
    path.with_file_name(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        counter
    ))
}

fn insert_artifact_row(
    tx: &Transaction<'_>,
    id: &ArtifactId,
    artifact: &NewArtifact,
    sha256: &str,
    relative_path: &str,
    size_bytes: usize,
) -> rusqlite::Result<ArtifactId> {
    tx.execute(
        "INSERT INTO artifacts (
            id, session_id, part_id, kind, mime, sha256, path, size_bytes, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(sha256) DO NOTHING",
        params![
            id.as_str(),
            artifact.session_id.as_str(),
            artifact.part_id.as_ref().map(PartId::as_str),
            artifact.kind.as_str(),
            artifact.mime.as_str(),
            sha256,
            relative_path,
            size_bytes as i64,
            artifact.created_at,
        ],
    )?;

    let stored_id = tx.query_row(
        "SELECT id FROM artifacts WHERE sha256 = ?1",
        [sha256],
        |row| row.get::<_, String>(0),
    )?;
    Ok(ArtifactId::new_unchecked(stored_id))
}

fn new_artifact_id() -> ArtifactId {
    static NEXT_ARTIFACT_ID: AtomicU64 = AtomicU64::new(0);

    let millis = unix_millis() as u64;
    let counter = NEXT_ARTIFACT_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::from(elapsed.subsec_nanos()))
        .unwrap_or(0);
    let entropy = (u64::from(std::process::id()) << 32) ^ counter ^ nanos;
    ArtifactId::new_unchecked(format!("art_{millis:016x}_{entropy:016x}"))
}

fn new_provider_payload_id() -> ProviderPayloadId {
    static NEXT_PROVIDER_PAYLOAD_ID: AtomicU64 = AtomicU64::new(0);

    let millis = unix_millis() as u64;
    let counter = NEXT_PROVIDER_PAYLOAD_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::from(elapsed.subsec_nanos()))
        .unwrap_or(0);
    let entropy = (u64::from(std::process::id()) << 32) ^ counter ^ nanos;
    ProviderPayloadId::new_unchecked(format!("pay_{millis:016x}_{entropy:016x}"))
}

fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn artifact_row_from_sql(row: &Row<'_>) -> rusqlite::Result<ArtifactRow> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let part_id: Option<String> = row.get(2)?;
    let size_bytes: i64 = row.get(7)?;

    Ok(ArtifactRow {
        id: ArtifactId::new_unchecked(id),
        session_id: SessionId::new_unchecked(session_id),
        part_id: part_id.map(PartId::new_unchecked),
        kind: row.get(3)?,
        mime: row.get(4)?,
        sha256: row.get(5)?,
        path: row.get(6)?,
        size_bytes: size_bytes as u64,
        created_at: row.get(8)?,
    })
}

fn read_provider_payload_row(row: &Row<'_>) -> rusqlite::Result<ProviderPayloadRow> {
    let id: String = row.get("id")?;
    let session_id: String = row.get("session_id")?;
    let run_id: String = row.get("run_id")?;
    let sequence: i64 = row.get("sequence")?;
    let artifact_id: String = row.get("artifact_id")?;

    Ok(ProviderPayloadRow {
        id: ProviderPayloadId::new_unchecked(id),
        session_id: SessionId::new_unchecked(session_id),
        run_id: RunId::new_unchecked(run_id),
        direction: row.get("direction")?,
        api_kind: row.get("api_kind")?,
        provider_id: row.get("provider_id")?,
        model_id: row.get("model_id")?,
        sequence: sequence as u32,
        provider_payload_id: row.get("provider_payload_id")?,
        artifact_id: ArtifactId::new_unchecked(artifact_id),
        sha256: row.get("sha256")?,
        decoder_version: row.get("decoder_version")?,
        decode_status: row.get("decode_status")?,
        error_json: row.get("error_json")?,
        created_at: row.get("created_at")?,
        decoded_at: row.get("decoded_at")?,
    })
}

fn read_provider_state(row: &Row<'_>) -> rusqlite::Result<ProviderState> {
    let run_id: String = row.get("run_id")?;
    Ok(ProviderState {
        run_id: RunId::new_unchecked(run_id),
        api_kind: row.get("api_kind")?,
        state_json: row.get("state_json")?,
    })
}

/// Read a single session row by id from any connection (locked handle or open
/// transaction). Returns `Ok(None)` when no such session exists.
fn read_session_row_by_id(
    conn: &Connection,
    session_id: &SessionId,
) -> Result<Option<SessionRow>, SqliteStoreError> {
    conn.query_row(
        r#"
        SELECT
            id,
            title,
            source,
            workspace_root,
            system_prompt,
            settings_json,
            parent_id,
            version,
            slug,
            cost,
            tokens_input,
            tokens_output,
            tokens_reasoning,
            tokens_cache_read,
            tokens_cache_write,
            time_archived,
            time_compacting,
            revert_json,
            created_at,
            updated_at
        FROM sessions
        WHERE id = ?1
        "#,
        [session_id.as_str()],
        read_session_row,
    )
    .optional()
    .map_err(read_query_err)
}

/// Read a single run row by id from any connection. Returns `Ok(None)` when no
/// such run exists.
fn read_run_row_by_id(
    conn: &Connection,
    run_id: &RunId,
) -> Result<Option<RunRow>, SqliteStoreError> {
    conn.query_row(
        r#"
        SELECT
            id,
            session_id,
            status,
            trigger,
            started_at,
            finished_at,
            error_json
        FROM runs
        WHERE id = ?1
        "#,
        [run_id.as_str()],
        read_run_row,
    )
    .optional()
    .map_err(read_query_err)
}

/// Read `source`'s turns (with parts) in chronological order, truncated to
/// include `through_message` when given. This is the read half of
/// [`SqliteSessionStore::fork_session`]; it runs against the open fork
/// transaction so the snapshot is consistent with the inserts. Errors with
/// `NotFound` when `through_message` is not part of the session.
fn read_turns_for_fork(
    conn: &Connection,
    source: &SessionId,
    through_message: Option<&MessageId>,
) -> Result<Vec<StoredTurn>, SqliteStoreError> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT t.id, t.run_id, t.seq, t.role, t.meta_json, t.created_at
            FROM turns t
            JOIN runs r ON r.id = t.run_id
            WHERE r.session_id = ?1
            ORDER BY t.created_at ASC, t.id ASC
            "#,
        )
        .map_err(read_query_err)?;
    let turns = stmt
        .query_map([source.as_str()], read_turn)
        .map_err(read_query_err)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(read_query_err)?;
    drop(stmt);
    let mut turns = collect_parts_for_turns(conn, turns)?;

    let Some(through_message) = through_message else {
        return Ok(turns);
    };
    let cutoff = turns
        .iter()
        .position(|(turn, _)| turn.id == *through_message)
        .ok_or_else(|| SqliteStoreError::NotFound {
            entity: "message",
            id: through_message.to_string(),
        })?;
    turns.truncate(cutoff + 1);
    Ok(turns)
}

/// Stash a typed error for [`SqliteSessionStore::fork_session`] to restore after
/// the write transaction, and return a sentinel `rusqlite::Error` so the
/// transaction rolls back. The sentinel text never surfaces — the caller
/// returns the stashed error instead.
fn fork_abort<T>(
    slot: &mut Option<SqliteStoreError>,
    error: SqliteStoreError,
) -> rusqlite::Result<T> {
    *slot = Some(error);
    // Any non-busy error rolls the transaction back without a retry. The text
    // is never read: the caller restores the stashed error from `slot`.
    Err(rusqlite::Error::InvalidQuery)
}

fn read_session_row(row: &Row<'_>) -> rusqlite::Result<SessionRow> {
    let id: String = row.get("id")?;
    let parent_id: Option<String> = row.get("parent_id")?;
    Ok(SessionRow {
        id: SessionId::new_unchecked(id),
        title: row.get("title")?,
        source: row.get("source")?,
        workspace_root: row.get("workspace_root")?,
        system_prompt: row.get("system_prompt")?,
        settings_json: row.get("settings_json")?,
        parent_id: parent_id.map(SessionId::new_unchecked),
        version: row.get("version")?,
        slug: row.get("slug")?,
        cost: row.get("cost")?,
        tokens_input: row.get("tokens_input")?,
        tokens_output: row.get("tokens_output")?,
        tokens_reasoning: row.get("tokens_reasoning")?,
        tokens_cache_read: row.get("tokens_cache_read")?,
        tokens_cache_write: row.get("tokens_cache_write")?,
        time_archived: row.get("time_archived")?,
        time_compacting: row.get("time_compacting")?,
        revert_json: row.get("revert_json")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn read_run_row(row: &Row<'_>) -> rusqlite::Result<RunRow> {
    let id: String = row.get("id")?;
    let session_id: String = row.get("session_id")?;
    Ok(RunRow {
        id: RunId::new_unchecked(id),
        session_id: SessionId::new_unchecked(session_id),
        status: row.get("status")?,
        trigger: row.get("trigger")?,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
        error_json: row.get("error_json")?,
    })
}

fn read_turn(row: &Row<'_>) -> rusqlite::Result<Turn> {
    let id: String = row.get("id")?;
    let run_id: String = row.get("run_id")?;
    let role: String = row.get("role")?;
    let meta_json: String = row.get("meta_json")?;
    Ok(Turn {
        id: MessageId::new_unchecked(id),
        run_id: RunId::new_unchecked(run_id),
        seq: row.get("seq")?,
        role: parse_turn_role(&role)?,
        meta: parse_json(&meta_json)?,
        created_at: row.get("created_at")?,
    })
}

fn read_stored_part(row: &Row<'_>) -> rusqlite::Result<StoredPart> {
    let id: String = row.get("id")?;
    let data_json: String = row.get("data_json")?;
    let provider_payload_id: Option<String> = row.get("provider_payload_id")?;
    Ok(StoredPart {
        id: PartId::new_unchecked(id),
        part: parse_json(&data_json)?,
        provider_payload_id: provider_payload_id.map(ProviderPayloadId::new_unchecked),
        provider_json_pointer: row.get("provider_json_pointer")?,
        compacted_at: row.get("compacted_at")?,
        created_at: row.get("created_at")?,
    })
}

fn parse_turn_role(value: &str) -> rusqlite::Result<TurnRole> {
    match value {
        "user" => Ok(TurnRole::User),
        "assistant" => Ok(TurnRole::Assistant),
        other => Err(from_sql_error(format!("unknown turn role `{other}`"))),
    }
}

fn parse_json<T>(value: &str) -> rusqlite::Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_str(value).map_err(|err| from_sql_error(err.to_string()))
}

fn from_sql_error(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

fn read_err(err: rusqlite::Error, entity: &'static str, id: &str) -> SqliteStoreError {
    match err {
        rusqlite::Error::QueryReturnedNoRows => SqliteStoreError::NotFound {
            entity,
            id: id.to_string(),
        },
        other => SqliteStoreError::ReadFailed(other.to_string()),
    }
}

fn read_query_err(err: rusqlite::Error) -> SqliteStoreError {
    SqliteStoreError::ReadFailed(err.to_string())
}

fn ensure_row_changed(
    changed: usize,
    entity: &'static str,
    id: &str,
) -> Result<(), SqliteStoreError> {
    if changed == 0 {
        Err(SqliteStoreError::NotFound {
            entity,
            id: id.to_string(),
        })
    } else {
        Ok(())
    }
}

fn invalid_run_status(operation: &'static str, status: RunStatus) -> SqliteStoreError {
    SqliteStoreError::InvalidRunStatus {
        operation,
        status: status.as_str().to_string(),
    }
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqliteStoreError {
    /// The database file could not be opened.
    OpenFailed(String),
    /// A pragma could not be applied during open.
    PragmaFailed(String),
    /// A locking-protocol failure (network filesystem); drives WAL fallback.
    Locking(String),
    /// A write transaction failed or exhausted its retry budget.
    WriteFailed(String),
    /// A read query failed.
    ReadFailed(String),
    /// Schema migration or reconciliation failed.
    MigrationFailed(String),
    /// A requested row does not exist.
    NotFound { entity: &'static str, id: String },
    /// The requested run status is not valid for this operation.
    InvalidRunStatus {
        operation: &'static str,
        status: String,
    },
    /// A run has already reached a terminal status and cannot transition again.
    InvalidRunTransition { id: String, status: String },
    /// An artifact row was not found.
    ArtifactNotFound(String),
    /// Artifact bytes could not be written to disk.
    BlobWriteFailed(String),
    /// Artifact bytes could not be opened from disk.
    BlobReadFailed(String),
}

impl std::fmt::Display for SqliteStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenFailed(msg) => write!(f, "open failed: {msg}"),
            Self::PragmaFailed(msg) => write!(f, "pragma failed: {msg}"),
            Self::Locking(msg) => write!(f, "locking protocol error: {msg}"),
            Self::WriteFailed(msg) => write!(f, "write failed: {msg}"),
            Self::ReadFailed(msg) => write!(f, "read failed: {msg}"),
            Self::MigrationFailed(msg) => write!(f, "migration failed: {msg}"),
            Self::NotFound { entity, id } => write!(f, "{entity} not found: {id}"),
            Self::InvalidRunStatus { operation, status } => {
                write!(f, "invalid run status for {operation}: {status}")
            }
            Self::InvalidRunTransition { id, status } => {
                write!(f, "run is already terminal: {id} is {status}")
            }
            Self::ArtifactNotFound(id) => write!(f, "artifact not found: {id}"),
            Self::BlobWriteFailed(msg) => write!(f, "blob write failed: {msg}"),
            Self::BlobReadFailed(msg) => write!(f, "blob read failed: {msg}"),
        }
    }
}

impl std::error::Error for SqliteStoreError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn mint_uuid_v7_is_monotonic_within_one_timestamp() {
        let timestamp = 1_700_000_000_000;
        let ids: Vec<String> = (0..1_000).map(|_| mint_uuid_v7(timestamp)).collect();

        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(
            ids, sorted,
            "ids minted with one timestamp must sort in allocation order"
        );

        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "minted ids must be unique");

        for id in &ids {
            assert!(
                nav_types::SessionId::try_new(id.clone()).is_ok(),
                "minted id {id} must be a valid UUIDv7"
            );
        }
    }

    /// A unique temp database path that removes the file and its WAL sidecars
    /// (`-wal`, `-shm`) on drop — even if the test panics. Declare it before the
    /// store so the connection closes before the files are removed.
    struct TempDb {
        path: std::path::PathBuf,
    }

    impl TempDb {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nav-sqlite-{name}-{}-{}.db",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut name = self.path.clone().into_os_string();
                name.push(suffix);
                let _ = std::fs::remove_file(std::path::PathBuf::from(name));
            }
        }
    }

    struct TempDataDir {
        path: std::path::PathBuf,
    }

    impl TempDataDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nav-sqlite-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir(&path).expect("temp data dir should be created");
            Self { path }
        }

        fn db_path(&self) -> std::path::PathBuf {
            self.path.join("nav.db")
        }
    }

    impl Drop for TempDataDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn insert_test_session(store: &SqliteSessionStore, session_id: &nav_types::SessionId) {
        store
            .execute_write(|tx| {
                tx.execute(
                    "INSERT INTO sessions (id, version, created_at, updated_at)
                     VALUES (?1, 'test', 1, 1)",
                    [session_id.as_str()],
                )
            })
            .expect("session setup");
    }

    #[test]
    fn relative_database_paths_capture_absolute_data_dir() {
        let data_dir = data_dir_for(Path::new("nav.db"));

        assert!(data_dir.is_absolute());
        assert_eq!(
            data_dir,
            std::env::current_dir().expect("current dir should be readable")
        );
    }

    #[test]
    fn artifact_round_trip_returns_metadata_and_exact_bytes() {
        let data_dir = TempDataDir::new("artifact-round-trip");
        let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
        let session_id =
            nav_types::SessionId::new_unchecked("019f2f6f-f178-7a72-9f28-000000000001");
        insert_test_session(&store, &session_id);

        let artifact_id = store
            .put_artifact(
                NewArtifact {
                    session_id: session_id.clone(),
                    part_id: None,
                    kind: ArtifactKind::ToolOutput,
                    mime: "text/plain".to_string(),
                    created_at: 123,
                },
                b"hello artifact bytes",
            )
            .expect("put artifact");

        let mut artifact = store
            .get_artifact(&artifact_id)
            .expect("artifact should be readable");
        let mut bytes = Vec::new();
        artifact
            .reader
            .read_to_end(&mut bytes)
            .expect("artifact reader should stream bytes");

        assert_eq!(artifact.row.id, artifact_id);
        assert_eq!(artifact.row.session_id, session_id);
        assert_eq!(artifact.row.kind, "tool_output");
        assert_eq!(artifact.row.mime, "text/plain");
        assert_eq!(artifact.row.size_bytes, 20);
        assert_eq!(bytes, b"hello artifact bytes");
    }

    #[test]
    fn duplicate_artifact_put_returns_existing_id() {
        let data_dir = TempDataDir::new("artifact-dedup");
        let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
        let session_id =
            nav_types::SessionId::new_unchecked("019f2f6f-f178-7a72-9f28-000000000001");
        insert_test_session(&store, &session_id);

        let first_id = store
            .put_artifact(
                NewArtifact {
                    session_id: session_id.clone(),
                    part_id: None,
                    kind: ArtifactKind::ToolOutput,
                    mime: "text/plain".to_string(),
                    created_at: 123,
                },
                b"same bytes",
            )
            .expect("first put");
        let second_id = store
            .put_artifact(
                NewArtifact {
                    session_id,
                    part_id: None,
                    kind: ArtifactKind::Other,
                    mime: "application/octet-stream".to_string(),
                    created_at: 124,
                },
                b"same bytes",
            )
            .expect("second put");

        assert_eq!(second_id, first_id);
    }

    #[test]
    fn missing_artifact_blob_is_a_hard_read_error() {
        let data_dir = TempDataDir::new("artifact-missing-blob");
        let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
        let session_id =
            nav_types::SessionId::new_unchecked("019f2f6f-f178-7a72-9f28-000000000001");
        insert_test_session(&store, &session_id);

        let artifact_id = store
            .put_artifact(
                NewArtifact {
                    session_id,
                    part_id: None,
                    kind: ArtifactKind::ToolOutput,
                    mime: "text/plain".to_string(),
                    created_at: 123,
                },
                b"bytes that should disappear",
            )
            .expect("put artifact");
        let artifact = store
            .get_artifact(&artifact_id)
            .expect("artifact should be readable before deletion");
        let blob_path = data_dir.path.join(&artifact.row.path);
        drop(artifact);

        std::fs::remove_file(blob_path).expect("blob should be removed");

        let err = store
            .get_artifact(&artifact_id)
            .expect_err("missing blob must not read as empty bytes");
        assert!(
            err.to_string().contains("blob read failed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn corrupt_artifact_blob_is_a_hard_read_error() {
        let data_dir = TempDataDir::new("artifact-corrupt-read");
        let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
        let session_id =
            nav_types::SessionId::new_unchecked("019f2f6f-f178-7a72-9f28-000000000001");
        insert_test_session(&store, &session_id);

        let artifact_id = store
            .put_artifact(
                NewArtifact {
                    session_id,
                    part_id: None,
                    kind: ArtifactKind::ToolOutput,
                    mime: "text/plain".to_string(),
                    created_at: 123,
                },
                b"original bytes",
            )
            .expect("put artifact");
        let artifact = store
            .get_artifact(&artifact_id)
            .expect("artifact should be readable before corruption");
        let blob_path = data_dir.path.join(&artifact.row.path);
        drop(artifact);
        std::fs::write(blob_path, b"corrupt bytes").expect("blob should be overwritten");

        let err = store
            .get_artifact(&artifact_id)
            .expect_err("corrupt blob must not read as original bytes");
        assert!(
            err.to_string().contains("does not match stored sha256"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn put_artifact_repairs_corrupt_blob_at_sha256_path() {
        let data_dir = TempDataDir::new("artifact-corrupt-blob");
        let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
        let session_id =
            nav_types::SessionId::new_unchecked("019f2f6f-f178-7a72-9f28-000000000001");
        insert_test_session(&store, &session_id);
        let bytes = b"correct artifact bytes";
        let blob_path = data_dir
            .path
            .join(artifact_relative_path(&sha256_hex(bytes)));
        std::fs::create_dir_all(blob_path.parent().expect("blob should have parent"))
            .expect("blob parent should be created");
        std::fs::write(&blob_path, b"stale partial bytes").expect("corrupt blob setup");

        let artifact_id = store
            .put_artifact(
                NewArtifact {
                    session_id,
                    part_id: None,
                    kind: ArtifactKind::ToolOutput,
                    mime: "text/plain".to_string(),
                    created_at: 123,
                },
                bytes,
            )
            .expect("put artifact should repair blob");

        let mut artifact = store
            .get_artifact(&artifact_id)
            .expect("artifact should be readable");
        let mut stored_bytes = Vec::new();
        artifact
            .reader
            .read_to_end(&mut stored_bytes)
            .expect("artifact bytes should read");

        assert_eq!(stored_bytes, bytes);
    }

    #[test]
    fn open_uses_wal_journal_mode_on_a_regular_file() {
        let db = TempDb::new("wal");
        let store = SqliteSessionStore::open(db.path()).expect("open should succeed");

        assert_eq!(store.journal_mode(), JournalMode::Wal);
    }

    #[test]
    fn checkpoint_cadence_fires_every_interval() {
        assert!(!should_checkpoint(0));
        assert!(!should_checkpoint(1));
        assert!(!should_checkpoint(CHECKPOINT_INTERVAL - 1));
        assert!(should_checkpoint(CHECKPOINT_INTERVAL));
        assert!(!should_checkpoint(CHECKPOINT_INTERVAL + 1));
        assert!(should_checkpoint(CHECKPOINT_INTERVAL * 2));
    }

    #[test]
    fn writes_spanning_multiple_checkpoints_all_commit() {
        let db = TempDb::new("checkpoint-smoke");
        let store = SqliteSessionStore::open(db.path()).expect("open should succeed");

        store
            .execute_write(|tx| tx.execute("CREATE TABLE rows (id INTEGER PRIMARY KEY)", []))
            .expect("setup commit");

        // Cross several checkpoint boundaries to prove the periodic
        // wal_checkpoint never disrupts in-flight writes.
        let total = (CHECKPOINT_INTERVAL * 2 + 5) as usize;
        for _ in 0..total {
            store
                .execute_write(|tx| tx.execute("INSERT INTO rows DEFAULT VALUES", []))
                .expect("write commit");
        }

        let count: i64 = store
            .execute_write(|tx| tx.query_row("SELECT COUNT(*) FROM rows", [], |r| r.get(0)))
            .expect("count read");
        assert_eq!(count, total as i64);
    }

    #[test]
    fn concurrent_immediate_writers_do_not_convoy() {
        const WRITERS: usize = 10;
        const WRITES_EACH: usize = 20;

        let db = TempDb::new("convoy");
        let path = db.path().to_path_buf();

        // One connection sets up the shared table the writers contend over.
        let setup = SqliteSessionStore::open(&path).expect("open should succeed");
        setup
            .execute_write(|tx| tx.execute("CREATE TABLE hits (id INTEGER PRIMARY KEY)", []))
            .expect("setup should commit");
        drop(setup);

        let handles: Vec<_> = (0..WRITERS)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    // Each writer opens its OWN connection (separate file lock),
                    // so contention surfaces at the SQLite level — exactly what
                    // BEGIN IMMEDIATE + busy_timeout + retry must absorb.
                    let store = SqliteSessionStore::open(&path).expect("writer open");
                    for _ in 0..WRITES_EACH {
                        store
                            .execute_write(|tx| tx.execute("INSERT INTO hits DEFAULT VALUES", []))
                            .expect("concurrent write must not fail with SQLITE_BUSY");
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("writer thread panicked");
        }

        let reader = SqliteSessionStore::open(&path).expect("reader open");
        let total: i64 = reader
            .execute_write(|tx| tx.query_row("SELECT COUNT(*) FROM hits", [], |r| r.get(0)))
            .expect("count read");
        assert_eq!(total, (WRITERS * WRITES_EACH) as i64);
    }

    #[test]
    fn execute_write_commits_an_immediate_transaction_and_counts_writes() {
        let db = TempDb::new("execute-write");
        let store = SqliteSessionStore::open(db.path()).expect("open should succeed");

        assert_eq!(store.write_count(), 0);

        store
            .execute_write(|tx| {
                tx.execute("CREATE TABLE kv (k TEXT PRIMARY KEY, v INTEGER)", [])?;
                tx.execute("INSERT INTO kv (k, v) VALUES ('a', 1)", [])
            })
            .expect("write should commit");

        assert_eq!(store.write_count(), 1);

        let value: i64 = store
            .execute_write(|tx| tx.query_row("SELECT v FROM kv WHERE k = 'a'", [], |r| r.get(0)))
            .expect("read-in-write should commit");

        assert_eq!(value, 1);
        assert_eq!(store.write_count(), 2);
    }

    #[test]
    fn open_falls_back_to_delete_journal_when_wal_is_unavailable() {
        let db = TempDb::new("nfs-fallback");
        // Simulate an NFS-style "locking protocol" failure on the WAL pragma.
        let store = SqliteSessionStore::open_simulating_wal_failure(db.path())
            .expect("open should succeed via DELETE fallback");

        assert_eq!(store.journal_mode(), JournalMode::Delete);
        assert_eq!(store.pragma_i64("busy_timeout"), 5000);
    }

    #[test]
    fn locking_protocol_errors_are_recognised() {
        let locking = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_PROTOCOL),
            Some("locking protocol".to_string()),
        );
        assert!(is_locking_protocol_error(&locking));

        let other = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".to_string()),
        );
        assert!(!is_locking_protocol_error(&other));
    }

    #[test]
    fn open_applies_durability_and_concurrency_pragmas() {
        let db = TempDb::new("pragmas");
        let store = SqliteSessionStore::open(db.path()).expect("open should succeed");

        // synchronous=NORMAL is reported as 1, foreign_keys=ON as 1.
        assert_eq!(store.pragma_i64("synchronous"), 1);
        assert_eq!(store.pragma_i64("foreign_keys"), 1);
        assert_eq!(store.pragma_i64("busy_timeout"), 5000);
        assert_eq!(store.pragma_i64("cache_size"), -64000);
    }

    #[test]
    fn open_creates_the_core_session_schema_and_is_idempotent() {
        let db = TempDb::new("core-schema");

        SqliteSessionStore::open(db.path()).expect("first open should migrate");
        SqliteSessionStore::open(db.path()).expect("second open should be idempotent");

        let conn = Connection::open(db.path()).expect("schema should be readable");
        assert_table_exists(&conn, "schema_migrations");
        assert_table_exists(&conn, "sessions");
        assert_table_exists(&conn, "runs");
        assert_table_exists(&conn, "turns");
        assert_table_exists(&conn, "turn_parts");
        assert_table_exists(&conn, "artifacts");
        assert_table_exists(&conn, "provider_payloads");
        assert_table_exists(&conn, "provider_state");

        assert_index_exists(&conn, "idx_runs_session_started");
        assert_index_exists(&conn, "idx_turns_run_seq");
        assert_index_exists(&conn, "idx_turn_parts_turn_id");
        assert_index_exists(&conn, "idx_turn_parts_session_id");
        assert_index_exists(&conn, "idx_artifacts_sha256");
        assert_index_exists(&conn, "idx_provider_payloads_run_sequence");
        assert_index_exists(&conn, "idx_provider_payloads_session");

        let (version, applied_at): (i64, i64) = conn
            .query_row(
                "SELECT version, applied_at FROM schema_migrations",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("schema_migrations should be queryable");
        assert_eq!(version, migrate::SCHEMA_VERSION);
        assert!(applied_at > 0);
    }

    #[test]
    fn core_schema_accepts_turn_parts_without_provider_payload_reference() {
        let db = TempDb::new("core-schema-insert");

        SqliteSessionStore::open(db.path()).expect("open should migrate");

        let conn = Connection::open(db.path()).expect("schema should be readable");
        conn.pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys should be enabled");
        conn.execute(
            "INSERT INTO sessions (id, version, created_at, updated_at) VALUES ('s1', 'test', 1, 1)",
            [],
        )
        .expect("session insert");
        conn.execute(
            "INSERT INTO runs (id, session_id, status, started_at) VALUES ('r1', 's1', 'running', 1)",
            [],
        )
        .expect("run insert");
        conn.execute(
            "INSERT INTO turns (id, run_id, seq, role, created_at) VALUES ('m1', 'r1', 0, 'user', 1)",
            [],
        )
        .expect("turn insert");
        conn.execute(
            "INSERT INTO turn_parts (id, turn_id, session_id, type, data_json, created_at)
             VALUES ('prt_1', 'm1', 's1', 'text', '{}', 1)",
            [],
        )
        .expect("turn part insert");
    }

    #[test]
    fn open_readds_missing_nullable_columns() {
        let db = TempDb::new("schema-reconcile");
        let conn = Connection::open(db.path()).expect("setup should open");
        create_sessions_table(&conn, None);
        drop(conn);

        SqliteSessionStore::open(db.path()).expect("open should reconcile missing nullable column");

        let conn = Connection::open(db.path()).expect("schema should be readable");
        assert_column_exists(&conn, "sessions", "slug");
    }

    #[test]
    fn open_rejects_missing_required_columns() {
        let db = TempDb::new("schema-missing-required");
        let conn = Connection::open(db.path()).expect("setup should open");
        create_sessions_table_with_overrides(
            &conn,
            "id              TEXT PRIMARY KEY NOT NULL",
            None,
            Some("slug            TEXT"),
        );
        drop(conn);

        let err = SqliteSessionStore::open(db.path())
            .expect_err("open should reject incompatible schema");

        assert!(
            err.to_string()
                .contains("missing required column sessions.source"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn open_rejects_incompatible_column_types() {
        let db = TempDb::new("schema-incompatible");
        let conn = Connection::open(db.path()).expect("setup should open");
        create_sessions_table(&conn, Some("slug            INTEGER"));
        drop(conn);

        let err = SqliteSessionStore::open(db.path())
            .expect_err("open should reject incompatible schema");

        assert!(
            err.to_string()
                .contains("incompatible column sessions.slug: expected TEXT, got INTEGER"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn failed_migration_does_not_partially_create_core_tables() {
        let db = TempDb::new("schema-failed-atomic");
        let conn = Connection::open(db.path()).expect("setup should open");
        create_sessions_table(&conn, Some("slug            INTEGER"));
        drop(conn);

        SqliteSessionStore::open(db.path()).expect_err("open should reject incompatible schema");

        let conn = Connection::open(db.path()).expect("schema should be readable");
        assert_table_exists(&conn, "sessions");
        assert_table_missing(&conn, "schema_migrations");
        assert_table_missing(&conn, "runs");
        assert_table_missing(&conn, "turns");
        assert_table_missing(&conn, "turn_parts");
        assert_table_missing(&conn, "artifacts");
        assert_table_missing(&conn, "provider_payloads");
        assert_table_missing(&conn, "provider_state");
    }

    #[test]
    fn open_rejects_incompatible_primary_keys() {
        let db = TempDb::new("schema-incompatible-primary-key");
        let conn = Connection::open(db.path()).expect("setup should open");
        create_sessions_table_with_overrides(
            &conn,
            "id              TEXT NOT NULL",
            Some("source          TEXT NOT NULL DEFAULT 'cli'"),
            Some("slug            TEXT"),
        );
        drop(conn);

        let err = SqliteSessionStore::open(db.path())
            .expect_err("open should reject incompatible schema");

        assert!(
            err.to_string().contains(
                "incompatible column sessions.id: expected PRIMARY KEY, got not primary key"
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn open_rejects_incompatible_defaults() {
        let db = TempDb::new("schema-incompatible-default");
        let conn = Connection::open(db.path()).expect("setup should open");
        create_sessions_table_with_overrides(
            &conn,
            "id              TEXT PRIMARY KEY NOT NULL",
            Some("source          TEXT NOT NULL DEFAULT 'api'"),
            Some("slug            TEXT"),
        );
        drop(conn);

        let err = SqliteSessionStore::open(db.path())
            .expect_err("open should reject incompatible schema");

        assert!(
            err.to_string()
                .contains("incompatible column sessions.source: expected 'cli', got 'api'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn open_rejects_case_changed_string_defaults() {
        let db = TempDb::new("schema-incompatible-default-case");
        let conn = Connection::open(db.path()).expect("setup should open");
        create_sessions_table_with_overrides(
            &conn,
            "id              TEXT PRIMARY KEY NOT NULL",
            Some("source          TEXT NOT NULL DEFAULT 'CLI'"),
            Some("slug            TEXT"),
        );
        drop(conn);

        let err = SqliteSessionStore::open(db.path())
            .expect_err("open should reject incompatible schema");

        assert!(
            err.to_string()
                .contains("incompatible column sessions.source: expected 'cli', got 'CLI'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn open_rejects_incompatible_schema_migrations_table() {
        let db = TempDb::new("schema-incompatible-migrations");
        let conn = Connection::open(db.path()).expect("setup should open");
        conn.execute_batch(
            r#"
            CREATE TABLE schema_migrations (
                version     TEXT PRIMARY KEY NOT NULL,
                applied_at  INTEGER NOT NULL
            );
            "#,
        )
        .expect("setup incompatible migrations table");
        drop(conn);

        let err = SqliteSessionStore::open(db.path())
            .expect_err("open should reject incompatible schema");

        assert!(
            err.to_string().contains(
                "incompatible column schema_migrations.version: expected INTEGER, got TEXT"
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn open_rejects_incompatible_indexes() {
        let db = TempDb::new("schema-incompatible-index");
        let conn = Connection::open(db.path()).expect("setup should open");
        create_sessions_table(&conn, Some("slug            TEXT"));
        conn.execute_batch(
            r#"
            CREATE TABLE runs (
                id              TEXT PRIMARY KEY NOT NULL,
                session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                status          TEXT NOT NULL,
                trigger         TEXT,
                started_at      INTEGER NOT NULL,
                finished_at     INTEGER,
                error_json      TEXT
            );

            CREATE INDEX idx_runs_session_started ON runs(status);
            "#,
        )
        .expect("setup incompatible index");
        drop(conn);

        let err = SqliteSessionStore::open(db.path())
            .expect_err("open should reject incompatible schema");

        assert!(
            err.to_string()
                .contains("incompatible index idx_runs_session_started"),
            "unexpected error: {err}"
        );
    }

    fn create_sessions_table(conn: &Connection, slug_column: Option<&str>) {
        create_sessions_table_with_overrides(
            conn,
            "id              TEXT PRIMARY KEY NOT NULL",
            Some("source          TEXT NOT NULL DEFAULT 'cli'"),
            slug_column,
        );
    }

    fn create_sessions_table_with_overrides(
        conn: &Connection,
        id_column: &str,
        source_column: Option<&str>,
        slug_column: Option<&str>,
    ) {
        let source_column = source_column
            .map(|column| format!("                {column},\n"))
            .unwrap_or_default();
        let slug_column = slug_column
            .map(|column| format!("                {column},\n"))
            .unwrap_or_default();
        let sql = format!(
            r#"
            CREATE TABLE sessions (
                {id_column},
                title           TEXT,
{source_column}                workspace_root  TEXT,
                system_prompt   TEXT,
                settings_json   TEXT NOT NULL DEFAULT '{{}}',
                parent_id       TEXT REFERENCES sessions(id),
                version         TEXT NOT NULL,
{slug_column}                cost            REAL NOT NULL DEFAULT 0,
                tokens_input    INTEGER NOT NULL DEFAULT 0,
                tokens_output   INTEGER NOT NULL DEFAULT 0,
                tokens_reasoning INTEGER NOT NULL DEFAULT 0,
                tokens_cache_read  INTEGER NOT NULL DEFAULT 0,
                tokens_cache_write INTEGER NOT NULL DEFAULT 0,
                time_archived   INTEGER,
                time_compacting INTEGER,
                revert_json     TEXT,
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL
            );
            "#
        );
        conn.execute_batch(&sql).expect("setup sessions table");
    }

    fn assert_table_exists(conn: &Connection, table: &str) {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .expect("sqlite_master should be queryable");
        assert_eq!(count, 1, "missing table {table}");
    }

    fn assert_table_missing(conn: &Connection, table: &str) {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .expect("sqlite_master should be queryable");
        assert_eq!(count, 0, "unexpected table {table}");
    }

    fn assert_index_exists(conn: &Connection, index: &str) {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [index],
                |row| row.get(0),
            )
            .expect("sqlite_master should be queryable");
        assert_eq!(count, 1, "missing index {index}");
    }

    fn assert_column_exists(conn: &Connection, table: &str, column: &str) {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("table_info should prepare");
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("table_info should query")
            .collect::<Result<Vec<_>, _>>()
            .expect("table_info rows should decode");
        assert!(
            columns.iter().any(|name| name == column),
            "missing column {table}.{column}"
        );
    }
}
