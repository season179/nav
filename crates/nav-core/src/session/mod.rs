//! SQLite-backed session storage.
//!
//! A `SessionStore` owns one connection (guarded by a `Mutex`) into a single
//! database file. The first call to `open` applies `init.sql` and records the
//! migration in `schema_version`; subsequent calls are idempotent because every
//! `CREATE` statement uses `IF NOT EXISTS`.
//!
//! Cost is never derived from token counts. Every turn is recorded with
//! `cost_source = 'unreported'` and `cost_micros = NULL` unless the caller
//! passes a [`ReportedCost`] obtained from a provider that actually reports it.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs};
use ulid::Ulid;

use crate::agent::{AgentEvent, TurnUsage};

/// The schema embedded into the binary; applied once on first open.
pub const INIT_SQL: &str = include_str!("init.sql");

/// Current migration version. Bump (and add a new migration) whenever the
/// schema changes incompatibly.
pub const SCHEMA_VERSION: i64 = 2;

/// `provider` value stored on every session created from `run_agent`. There is
/// no `ModelProvider` trait yet; that arrives in a later slice.
pub const PROVIDER_OPENAI_RESPONSES: &str = "openai-responses";

/// Currency used for both `session.cost_currency` and `turn.cost_currency`
/// when the provider does not report one. Matches the `DEFAULT 'USD'` in
/// `init.sql`.
pub const DEFAULT_CURRENCY: &str = "USD";

/// `turn.cost_source` value indicating the provider returned a cost figure.
pub const COST_SOURCE_REPORTED: &str = "reported";

/// `turn.cost_source` value indicating cost was not reported. nav-core never
/// derives cost from `tokens × pricing`, so unreported stays unreported.
pub const COST_SOURCE_UNREPORTED: &str = "unreported";

/// Newtype-style alias for the ULID strings used as session primary keys.
pub type SessionId = String;

/// A cost figure reported by a provider. nav-core never synthesises this from
/// token counts; absence here is recorded as `cost_source = 'unreported'`.
#[derive(Debug, Clone)]
pub struct ReportedCost {
    pub micros: i64,
    pub currency: String,
}

/// Rolling token totals for one session. Returned by
/// [`SessionStore::session_token_totals`] so the agent loop can decide whether
/// automatic compaction should fire before submitting the next turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionTokenTotals {
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_input_cached: u64,
    pub tokens_reasoning: u64,
}

/// One row returned by [`SessionStore::list_sessions`], with the rollup fields
/// the CLI's `--list-sessions` command formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub name: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_active: i64,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub first_user_prompt: Option<String>,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_input_cached: u64,
    pub tokens_reasoning: u64,
    pub cost_micros_reported: i64,
    pub turns_with_reported_cost: u64,
    pub turns_total: u64,
    pub turn_count: u64,
    pub cost_currency: String,
}

/// Transcript export formats supported by both the TUI `/export` command and
/// the headless `nav export` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Json,
}

/// Error returned when resolving a user-supplied session ULID or prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveSessionError {
    NotFound {
        query: String,
    },
    AmbiguousPrefix {
        prefix: String,
        matches: Vec<String>,
    },
}

impl fmt::Display for ResolveSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { query } => write!(f, "session not found: {query}"),
            Self::AmbiguousPrefix { prefix, matches } => {
                write!(
                    f,
                    "session prefix {prefix:?} is ambiguous: {}",
                    matches.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for ResolveSessionError {}

/// Pick an export format from an explicit override or a path extension.
/// Unknown/missing extensions default to Markdown.
pub fn infer_export_format(path: Option<&Path>, explicit: Option<ExportFormat>) -> ExportFormat {
    if let Some(format) = explicit {
        return format;
    }
    let Some(path) = path else {
        return ExportFormat::Markdown;
    };
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("json") => ExportFormat::Json,
        _ => ExportFormat::Markdown,
    }
}

/// Render a durable `AgentEvent` stream as either Markdown or JSON. JSON uses
/// the existing serde shape directly: one array element per `AgentEvent`.
pub fn export_events(events: &[AgentEvent], format: ExportFormat) -> Result<String> {
    match format {
        ExportFormat::Markdown => export_events_markdown(events),
        ExportFormat::Json => {
            serde_json::to_string_pretty(events).context("failed to serialize transcript JSON")
        }
    }
}

fn export_events_markdown(events: &[AgentEvent]) -> Result<String> {
    let mut out = String::from("# nav transcript\n\n");
    let mut turn = 0usize;
    let mut in_turn = false;
    let mut tool_names: HashMap<String, String> = HashMap::new();

    for event in events {
        match event {
            AgentEvent::UserMessage {
                text,
                display_text,
                attachments,
            } => {
                start_turn(&mut out, &mut turn, &mut in_turn);
                push_section(&mut out, "User", display_text.as_deref().unwrap_or(text));
                for attachment in attachments {
                    match attachment {
                        crate::agent::UserAttachment::Image { path } => {
                            out.push_str(&format!("- attachment: image `{}`\n", path.display()));
                        }
                        crate::agent::UserAttachment::File { path } => {
                            out.push_str(&format!("- attachment: file `{}`\n", path.display()));
                        }
                    }
                }
                if !attachments.is_empty() {
                    out.push('\n');
                }
            }
            AgentEvent::AssistantMessageDone { text } => {
                start_turn(&mut out, &mut turn, &mut in_turn);
                push_section(&mut out, "Assistant", text);
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                arguments,
            } => {
                start_turn(&mut out, &mut turn, &mut in_turn);
                tool_names.insert(call_id.clone(), name.clone());
                push_json_details(&mut out, &format!("Tool call: {name}"), arguments)?;
            }
            AgentEvent::ToolCallOutput {
                call_id,
                output,
                is_error,
            } => {
                start_turn(&mut out, &mut turn, &mut in_turn);
                let label = if *is_error {
                    "### Tool result (error)"
                } else {
                    "### Tool result"
                };
                out.push_str(label);
                if let Some(name) = tool_names.get(call_id) {
                    out.push_str(&format!(": {name}"));
                }
                out.push_str("\n\n");
                out.push_str("```text\n");
                out.push_str(output.trim_end_matches('\n'));
                out.push_str("\n```\n\n");
            }
            AgentEvent::Error { message } => {
                start_turn(&mut out, &mut turn, &mut in_turn);
                push_section(&mut out, "Error", message);
            }
            AgentEvent::CompactionCompleted { summary, .. } => {
                start_turn(&mut out, &mut turn, &mut in_turn);
                push_section(&mut out, "Compaction summary", summary);
            }
            AgentEvent::TurnComplete { .. } => {
                in_turn = false;
            }
            AgentEvent::TurnAborted { turn_id, reason } => {
                start_turn(&mut out, &mut turn, &mut in_turn);
                push_section(&mut out, "Turn aborted", &format!("{turn_id}: {reason}"));
                in_turn = false;
            }
            AgentEvent::AssistantMessageDelta { .. }
            | AgentEvent::ProviderRetry { .. }
            | AgentEvent::ContextTrimmed { .. }
            | AgentEvent::ToolCallApprovalRequest { .. }
            | AgentEvent::ToolCallBlocked { .. }
            | AgentEvent::PendingInputQueued { .. }
            | AgentEvent::PendingInputEdited { .. }
            | AgentEvent::PendingInputRemoved { .. }
            | AgentEvent::PendingInputCleared { .. }
            | AgentEvent::PendingInputDequeued { .. }
            | AgentEvent::FileChange { .. }
            | AgentEvent::TurnDiff { .. }
            | AgentEvent::CompactionStarted { .. }
            | AgentEvent::CompactionFailed { .. } => {
                start_turn(&mut out, &mut turn, &mut in_turn);
                push_json_details(&mut out, &format!("Event: {}", event.kind()), event)?;
            }
        }
    }

    Ok(out)
}

fn start_turn(out: &mut String, turn: &mut usize, in_turn: &mut bool) {
    if *in_turn {
        return;
    }
    *turn += 1;
    *in_turn = true;
    out.push_str(&format!("## Turn {turn}\n\n"));
}

fn push_section(out: &mut String, heading: &str, body: &str) {
    out.push_str(&format!("### {heading}\n\n"));
    out.push_str(body);
    out.push_str("\n\n");
}

fn push_json_details(out: &mut String, summary: &str, value: &impl Serialize) -> Result<()> {
    out.push_str("<details>\n");
    out.push_str(&format!("<summary>{summary}</summary>\n\n"));
    out.push_str("```json\n");
    out.push_str(&serde_json::to_string_pretty(value)?);
    out.push_str("\n```\n");
    out.push_str("</details>\n\n");
    Ok(())
}

/// Owns the single SQLite connection used to persist a nav session. All
/// methods are `&self` so callers can share the store across the agent loop
/// and the CLI without an extra `Arc<Mutex<…>>`.
pub struct SessionStore {
    conn: Mutex<Connection>,
}

impl SessionStore {
    /// Opens (and migrates) the session database. When `path` is `None` the
    /// XDG data directory — `$XDG_DATA_HOME/nav/nav.db`, falling back to
    /// `~/.local/share/nav/nav.db` — is used. Relative overrides are resolved
    /// inside that same nav data directory. Echoes the resolved pragma values
    /// to stderr so misconfiguration is visible at startup.
    pub fn open(path: Option<PathBuf>) -> Result<Self> {
        let path = resolve_db_path(path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;

        // `journal_mode = WAL` returns the new mode as a row; the other two
        // are set with execute_batch and then read back so the echoed
        // values reflect what SQLite actually accepted.
        let journal_mode: String =
            conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL")?;
        let synchronous: i64 = conn.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        let foreign_keys: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        eprintln!(
            "nav-core: opened {} (journal_mode={journal_mode}, synchronous={synchronous}, foreign_keys={foreign_keys})",
            path.display()
        );

        apply_schema(&conn)?;
        Ok(SessionStore {
            conn: Mutex::new(conn),
        })
    }

    /// Inserts a new `session` row and returns the generated ULID.
    pub fn create_session(
        &self,
        cwd: &Path,
        provider: &str,
        model: &str,
        profile: Option<&str>,
    ) -> Result<SessionId> {
        self.create_session_named(cwd, provider, model, profile, None)
    }

    /// Inserts a new `session` row with an optional human name and returns the
    /// generated ULID. Names are display metadata only; they are intentionally
    /// nullable and non-unique.
    pub fn create_session_named(
        &self,
        cwd: &Path,
        provider: &str,
        model: &str,
        profile: Option<&str>,
        name: Option<&str>,
    ) -> Result<SessionId> {
        let id = Ulid::new().to_string();
        let now = now_secs();
        let cwd_str = cwd.to_string_lossy().into_owned();
        let conn = self.lock();
        conn.execute(
            "INSERT INTO session (id, cwd, provider, model, profile, name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, cwd_str, provider, model, profile, name, now],
        )?;
        Ok(id)
    }

    /// Sets or replaces the display name for a session. Names are not unique;
    /// the ULID remains the stable identity for resume/export.
    pub fn set_session_name(&self, session_id: &str, name: &str) -> Result<()> {
        let trimmed = name.trim();
        anyhow::ensure!(!trimmed.is_empty(), "session name cannot be empty");
        let conn = self.lock();
        let updated = conn.execute(
            "UPDATE session SET name = ?1, updated_at = ?2 WHERE id = ?3",
            params![trimmed, now_secs(), session_id],
        )?;
        if updated == 0 {
            anyhow::bail!("session not found: {session_id}");
        }
        Ok(())
    }

    /// Persists a durable event to the session log and bumps `updated_at`.
    /// `AssistantMessageDelta` is intentionally dropped — it is a stream-only
    /// concern. When the event is a `TurnComplete`, the session's
    /// `tokens_*` rollups are incremented by the turn's usage.
    pub fn append_event(&self, session_id: &str, event: &AgentEvent) -> Result<()> {
        if !event.is_durable() {
            return Ok(());
        }
        let now = now_secs();
        let kind = event.kind();
        let data = serde_json::to_string(event).context("failed to serialize event")?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        // Folding `MAX(seq)+1` into the INSERT removes a per-event SELECT
        // round-trip. The subquery returns NULL for the first event in a
        // session, so COALESCE seeds seq at 0.
        tx.execute(
            "INSERT INTO event (session_id, seq, created_at, kind, data)
             VALUES (
                 ?1,
                 COALESCE((SELECT MAX(seq) FROM event WHERE session_id = ?1), -1) + 1,
                 ?2, ?3, ?4
             )",
            params![session_id, now, kind, data],
        )?;
        if let AgentEvent::TurnComplete { usage } = event {
            tx.execute(
                "UPDATE session
                 SET tokens_input = tokens_input + ?1,
                     tokens_output = tokens_output + ?2,
                     tokens_input_cached = tokens_input_cached + ?3,
                     tokens_reasoning = tokens_reasoning + ?4,
                     updated_at = ?5
                 WHERE id = ?6",
                params![
                    usage.tokens_input as i64,
                    usage.tokens_output as i64,
                    usage.tokens_input_cached as i64,
                    usage.tokens_reasoning as i64,
                    now,
                    session_id,
                ],
            )?;
        } else {
            tx.execute(
                "UPDATE session SET updated_at = ?1 WHERE id = ?2",
                params![now, session_id],
            )?;
        }
        // Mirror approval-related events into the side table so audits can
        // join on (session_id, approval_id) without scanning the event log.
        match event {
            AgentEvent::ToolCallApprovalRequest {
                approval_id,
                tool,
                command,
                path,
                reason,
                ..
            } => {
                let command_json = command
                    .as_ref()
                    .map(|c| serde_json::to_string(c).unwrap_or_default());
                tx.execute(
                    "INSERT OR IGNORE INTO approval
                     (session_id, approval_id, requested_at, tool, command, path, reason)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        session_id,
                        approval_id,
                        now,
                        tool,
                        command_json,
                        path,
                        reason
                    ],
                )?;
            }
            AgentEvent::ToolCallBlocked {
                call_id,
                tool,
                reason,
                rule,
            } => {
                // Use call_id as the audit key for blocks (no approval_id
                // was ever issued — the request was refused outright).
                tx.execute(
                    "INSERT OR IGNORE INTO approval
                     (session_id, approval_id, requested_at, tool, reason, rule)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![session_id, call_id, now, tool, reason, rule],
                )?;
            }
            _ => {}
        }
        tx.commit()?;
        Ok(())
    }

    /// Wrap this store in a [`DurableEventSink`] tied to one session. The
    /// returned handle clones the underlying `Arc<SessionStore>` so the
    /// caller can move it into `ChannelGate::with_sink` without juggling
    /// borrows.
    pub fn sink_for(
        self: &std::sync::Arc<Self>,
        session_id: impl Into<String>,
    ) -> SessionStoreSink {
        SessionStoreSink {
            store: std::sync::Arc::clone(self),
            session_id: session_id.into(),
        }
    }

    /// Record a user decision against a previously-requested approval.
    /// Mirrors the `approval_response` JSON consumed by the NDJSON gate.
    pub fn record_approval_decision(
        &self,
        session_id: &str,
        approval_id: &str,
        decision: &str,
    ) -> Result<()> {
        let now = now_secs();
        let conn = self.lock();
        conn.execute(
            "UPDATE approval
             SET decided_at = ?1, decision = ?2
             WHERE session_id = ?3 AND approval_id = ?4",
            params![now, decision, session_id, approval_id],
        )?;
        Ok(())
    }

    /// Records one turn in the `turn` table and rolls its outcome up into
    /// `session`. The cost columns reflect the constraint that nav never
    /// computes cost from `tokens × pricing`: tokens are stored regardless,
    /// but `cost_source` is `'reported'` only when `cost` is `Some`.
    pub fn complete_turn(
        &self,
        session_id: &str,
        model: &str,
        usage: &TurnUsage,
        cost: Option<ReportedCost>,
    ) -> Result<()> {
        let now = now_secs();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let (cost_micros, cost_currency, cost_source): (Option<i64>, &str, &str) = match &cost {
            Some(c) => (Some(c.micros), c.currency.as_str(), COST_SOURCE_REPORTED),
            None => (None, DEFAULT_CURRENCY, COST_SOURCE_UNREPORTED),
        };
        tx.execute(
            "INSERT INTO turn (
                session_id, turn_index, started_at, ended_at, model,
                tokens_input, tokens_output, tokens_input_cached, tokens_reasoning,
                cost_micros, cost_currency, cost_source
             ) VALUES (
                ?1,
                COALESCE((SELECT MAX(turn_index) FROM turn WHERE session_id = ?1), -1) + 1,
                ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
             )",
            params![
                session_id,
                now,
                now,
                model,
                usage.tokens_input as i64,
                usage.tokens_output as i64,
                usage.tokens_input_cached as i64,
                usage.tokens_reasoning as i64,
                cost_micros,
                cost_currency,
                cost_source,
            ],
        )?;
        // Cost-source rollups branch separately so unreported turns never
        // touch `cost_micros_reported` or `turns_with_reported_cost`.
        if let Some(c) = &cost {
            tx.execute(
                "UPDATE session
                 SET cost_micros_reported = cost_micros_reported + ?1,
                     turns_with_reported_cost = turns_with_reported_cost + 1,
                     turns_total = turns_total + 1,
                     updated_at = ?2
                 WHERE id = ?3",
                params![c.micros, now, session_id],
            )?;
        } else {
            tx.execute(
                "UPDATE session
                 SET turns_total = turns_total + 1,
                     updated_at = ?1
                 WHERE id = ?2",
                params![now, session_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Returns every durable event for a session in seq order. Each row's
    /// `data` column is parsed back into the `AgentEvent` it was serialised
    /// from, so callers can reconstruct the conversation transcript directly.
    ///
    /// Rows whose `kind` discriminant does not deserialize cleanly into the
    /// current [`AgentEvent`] enum are skipped with a stderr warning. This
    /// lets a newer nav write events a slightly older nav can still resume
    /// past — losing one row is less disruptive than failing the whole
    /// `--resume`.
    pub fn load_session(&self, session_id: &str) -> Result<Vec<AgentEvent>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT seq, kind, data FROM event WHERE session_id = ?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (seq, kind, data) = row?;
            match serde_json::from_str::<AgentEvent>(&data) {
                Ok(event) => events.push(event),
                Err(err) => {
                    eprintln!(
                        "nav-core: skipping unknown event (session={session_id} seq={seq} kind={kind}): {err}"
                    );
                }
            }
        }
        Ok(events)
    }

    /// Returns the `tokens_before` field from the most recent
    /// `compaction_completed` event recorded for `session_id`, or `None` if
    /// the session has never been compacted. Used as the baseline against
    /// which post-checkpoint usage is measured for automatic compaction —
    /// without it, once lifetime rolling tokens cross the threshold, every
    /// later prompt would re-compact.
    pub(crate) fn latest_checkpoint_tokens_before(&self, session_id: &str) -> Result<Option<u64>> {
        let conn = self.lock();
        let row: Option<String> = conn
            .query_row(
                "SELECT data FROM event
                 WHERE session_id = ?1 AND kind = 'compaction_completed'
                 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(data) = row else {
            return Ok(None);
        };
        let value: serde_json::Value = serde_json::from_str(&data)
            .context("failed to parse stored compaction_completed event")?;
        Ok(value
            .get("tokens_before")
            .and_then(serde_json::Value::as_u64))
    }

    /// Returns the rolling token totals recorded against `session_id`. Used
    /// by the compaction module to decide whether automatic compaction
    /// should fire before submitting the next turn.
    pub(crate) fn session_token_totals(&self, session_id: &str) -> Result<SessionTokenTotals> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT tokens_input, tokens_output, tokens_input_cached, tokens_reasoning
                 FROM session WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok(SessionTokenTotals {
                        tokens_input: row.get::<_, i64>(0)? as u64,
                        tokens_output: row.get::<_, i64>(1)? as u64,
                        tokens_input_cached: row.get::<_, i64>(2)? as u64,
                        tokens_reasoning: row.get::<_, i64>(3)? as u64,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        Ok(row)
    }

    /// Returns the launch cwd recorded for `session_id` at creation time.
    /// Replay uses this — not the resumed process's cwd — when resolving
    /// workspace-relative attachment paths back into bytes, otherwise an
    /// image saved during a session created in repo A would silently fail
    /// to attach when the user resumes from repo B.
    pub fn session_cwd(&self, session_id: &str) -> Result<PathBuf> {
        let conn = self.lock();
        let cwd: String = conn
            .query_row(
                "SELECT cwd FROM session WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .with_context(|| format!("session not found: {session_id}"))?;
        Ok(PathBuf::from(cwd))
    }

    /// Lists all sessions, sorted by `updated_at DESC`. Pass `cwd` to scope
    /// the listing to a single working directory.
    pub fn list_sessions(&self, cwd: Option<&Path>) -> Result<Vec<SessionSummary>> {
        let conn = self.lock();
        // `?1 IS NULL OR cwd = ?1` folds both the unfiltered and cwd-filtered
        // queries into one prepared statement; SQLite short-circuits the
        // disjunction so the index on `(cwd, updated_at DESC)` is still used
        // when a path is supplied.
        let cwd_str: Option<String> = cwd.map(|p| p.to_string_lossy().into_owned());
        let mut stmt = conn.prepare(
            "SELECT id, name, created_at, updated_at, cwd, provider, model,
                    tokens_input, tokens_output, tokens_input_cached, tokens_reasoning,
                    cost_micros_reported, turns_with_reported_cost, turns_total, cost_currency,
                    (
                        SELECT data FROM event
                        WHERE event.session_id = session.id AND kind = 'user_message'
                        ORDER BY seq ASC
                        LIMIT 1
                    ) AS first_user_event
             FROM session
             WHERE ?1 IS NULL OR cwd = ?1
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(params![cwd_str], |row| {
            let turns_total = row.get::<_, i64>(13)? as u64;
            Ok(SessionSummary {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                last_active: row.get(3)?,
                cwd: row.get(4)?,
                provider: row.get(5)?,
                model: row.get(6)?,
                tokens_input: row.get::<_, i64>(7)? as u64,
                tokens_output: row.get::<_, i64>(8)? as u64,
                tokens_input_cached: row.get::<_, i64>(9)? as u64,
                tokens_reasoning: row.get::<_, i64>(10)? as u64,
                cost_micros_reported: row.get(11)?,
                turns_with_reported_cost: row.get::<_, i64>(12)? as u64,
                turns_total,
                turn_count: turns_total,
                cost_currency: row.get(14)?,
                first_user_prompt: first_user_prompt_from_event_json(row.get(15)?),
            })
        })?;
        let mut summaries = Vec::new();
        for row in rows {
            summaries.push(row?);
        }
        Ok(summaries)
    }

    /// Resolves a full session ULID or unique prefix into the canonical ULID.
    pub fn resolve_session_id(
        &self,
        query: &str,
    ) -> std::result::Result<SessionId, ResolveSessionError> {
        let prefix = query.trim();
        if prefix.is_empty() {
            return Err(ResolveSessionError::NotFound {
                query: query.to_string(),
            });
        }
        let not_found = || ResolveSessionError::NotFound {
            query: prefix.to_string(),
        };
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM session WHERE id LIKE ?1 ORDER BY id ASC LIMIT 3")
            .map_err(|_| not_found())?;
        let rows = stmt
            .query_map(params![format!("{prefix}%")], |row| row.get::<_, String>(0))
            .map_err(|_| not_found())?;
        let mut matches: Vec<String> = rows
            .collect::<rusqlite::Result<_>>()
            .map_err(|_| not_found())?;
        match matches.len() {
            0 => Err(not_found()),
            1 => Ok(matches.remove(0)),
            _ => Err(ResolveSessionError::AmbiguousPrefix {
                prefix: prefix.to_string(),
                matches,
            }),
        }
    }

    /// Acquires the connection mutex. A panic here means another method
    /// previously panicked while holding the lock — the database is in an
    /// undefined state and the only safe recovery is to crash the process.
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("session store mutex poisoned")
    }
}

/// Resolves the on-disk location of the session database. Absolute overrides
/// are honored as-is; relative overrides mirror opencode's behavior by staying
/// under nav's per-user data directory instead of the launch cwd.
fn resolve_db_path(path: Option<PathBuf>) -> Result<PathBuf> {
    match path {
        Some(path) if path == Path::new(":memory:") || path.is_absolute() => Ok(path),
        Some(path) => Ok(default_db_dir()?.join(path)),
        None => default_db_path(),
    }
}

/// Resolves the per-user XDG data directory used for nav-owned durable storage.
fn default_db_dir() -> Result<PathBuf> {
    let base = xdg_data_home().context("could not resolve XDG data directory for nav.db")?;
    Ok(base.join("nav"))
}

/// Resolves the default on-disk location of the session database when the
/// caller does not pass an explicit path. Per spec: XDG data home joined with
/// `nav/nav.db`.
fn default_db_path() -> Result<PathBuf> {
    Ok(default_db_dir()?.join("nav.db"))
}

fn xdg_data_home() -> Option<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("share")))
}

fn apply_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(INIT_SQL)
        .context("failed to apply nav-core schema")?;
    record_schema_version(conn, 1)?;
    if applied_schema_version(conn)? < 2 {
        if !table_has_column(conn, "session", "name")? {
            conn.execute_batch("ALTER TABLE session ADD COLUMN name TEXT")?;
        }
        record_schema_version(conn, 2)?;
    }
    Ok(())
}

fn applied_schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, Option<i64>>(0)
        })?
        .unwrap_or(0))
}

fn record_schema_version(conn: &Connection, version: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (?1, ?2)",
        params![version, now_secs()],
    )?;
    Ok(())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn first_user_prompt_from_event_json(data: Option<String>) -> Option<String> {
    let data = data?;
    match serde_json::from_str::<AgentEvent>(&data).ok()? {
        AgentEvent::UserMessage {
            text, display_text, ..
        } => display_text.or(Some(text)),
        _ => None,
    }
}

/// [`DurableEventSink`] adaptor that writes through a [`SessionStore`].
///
/// The `ChannelGate` lives outside `run_agent`'s emit path, so without this
/// the approval-request event would only land on the live `events` channel
/// and never reach SQLite — leaving `record_approval_decision` updating
/// zero rows. Build one with [`SessionStore::sink_for`].
pub struct SessionStoreSink {
    store: std::sync::Arc<SessionStore>,
    session_id: String,
}

impl crate::permissions::approval::DurableEventSink for SessionStoreSink {
    fn persist(&self, event: &AgentEvent) {
        if let Err(err) = self.store.append_event(&self.session_id, event) {
            // Persistence is best-effort: a SQLite hiccup must not stall
            // the live conversation. Log once and continue.
            eprintln!("nav-core: failed to persist approval event: {err:#}");
        }
    }
}

impl crate::permissions::approval::DecisionRecorder for SessionStoreSink {
    fn record(&self, approval_id: &str, decision: crate::permissions::ReviewDecision) {
        if let Err(err) =
            self.store
                .record_approval_decision(&self.session_id, approval_id, decision.as_str())
        {
            eprintln!("nav-core: failed to record approval decision: {err:#}");
        }
    }
}

fn now_secs() -> i64 {
    // Unix epoch is non-negative for the next several billion years; the
    // cast cannot wrap for any realistic clock. If the clock is set before
    // 1970 (e.g. embedded board with no RTC) we record 0 instead of panicking.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
