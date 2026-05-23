//! SQLite-backed session storage.
//!
//! A `SessionStore` owns one connection (guarded by a `Mutex`) into a single
//! database file. The first call to `open` applies `init.sql` and records the
//! schema version in `schema_version`; subsequent calls are idempotent because
//! every `CREATE` statement uses `IF NOT EXISTS`.
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

use crate::agent_loop::{AgentEvent, TurnUsage, UserAttachment};

/// The schema embedded into the binary; applied once on first open.
pub const INIT_SQL: &str = include_str!("init.sql");

/// Current schema version. Bump when the fresh schema shape changes.
pub const SCHEMA_VERSION: i64 = 3;

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
    pub parent_id: Option<String>,
    pub labels: Vec<String>,
    pub child_count: u64,
}

/// One hit from [`SessionStore::search_transcript`]. The snippet is wrapped in
/// SQLite's FTS5 `snippet()` markers so the CLI/TUI can highlight matches
/// without re-running the tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptHit {
    pub session_id: String,
    pub seq: i64,
    pub kind: String,
    pub snippet: String,
    pub summary: SessionSummary,
}

pub use reference::ThreadReadOptions;

/// What [`SessionStore::rewind_to_user_message`] returns to the caller after
/// trimming the event log. The message fields are the original
/// `user_message` contents at the rewound seq so the next prompt can be
/// pre-filled in the composer — `display_text` mirrors the optional
/// renderer-only override stored on [`AgentEvent::UserMessage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindOutcome {
    pub target_seq: u64,
    pub removed_events: usize,
    pub text: String,
    pub display_text: Option<String>,
    pub attachments: Vec<UserAttachment>,
}

/// One node in the parent → child tree returned by [`SessionStore::walk_tree`].
/// `depth` is the distance from the root passed in (root itself is depth 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTreeNode {
    pub summary: SessionSummary,
    pub depth: u32,
}

/// Group `summaries` so each child sits immediately after its parent when the
/// parent is also in the input slice; otherwise treat the row as a root.
/// Returns `(depth, summary)` pairs with `depth = 0` for roots and orphans —
/// orphans appear at the end so nothing is silently dropped.
///
/// Shared between the CLI's `--list-sessions` formatter and the TUI's
/// `/sessions` cell so the two surfaces never drift on indentation rules.
pub fn layout_session_tree(summaries: &[SessionSummary]) -> Vec<(usize, &SessionSummary)> {
    use std::collections::{HashMap, HashSet};
    let ids: HashSet<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
    let mut children_by_parent: HashMap<&str, Vec<&SessionSummary>> = HashMap::new();
    let mut roots: Vec<&SessionSummary> = Vec::new();
    for summary in summaries {
        match summary.parent_id.as_deref() {
            Some(parent) if ids.contains(parent) => {
                children_by_parent.entry(parent).or_default().push(summary);
            }
            _ => roots.push(summary),
        }
    }
    fn walk<'a>(
        node: &'a SessionSummary,
        depth: usize,
        out: &mut Vec<(usize, &'a SessionSummary)>,
        children_by_parent: &mut HashMap<&'a str, Vec<&'a SessionSummary>>,
    ) {
        out.push((depth, node));
        if let Some(children) = children_by_parent.remove(node.id.as_str()) {
            for child in children {
                walk(child, depth + 1, out, children_by_parent);
            }
        }
    }
    let mut out = Vec::with_capacity(summaries.len());
    for root in roots {
        walk(root, 0, &mut out, &mut children_by_parent);
    }
    // Anything still in `children_by_parent` is an orphan whose parent isn't
    // visible (e.g. cwd-filtered out). Append at depth 0, newest first, so
    // the row count matches the input length.
    let mut leftover: Vec<&SessionSummary> = children_by_parent.into_values().flatten().collect();
    leftover.sort_by_key(|summary| std::cmp::Reverse(summary.updated_at));
    for summary in leftover {
        out.push((0, summary));
    }
    out
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
                        crate::agent_loop::UserAttachment::Image { path } => {
                            out.push_str(&format!("- attachment: image `{}`\n", path.display()));
                        }
                        crate::agent_loop::UserAttachment::File { path } => {
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
                ..
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
            | AgentEvent::ResponseContinuation { .. }
            | AgentEvent::ProviderRetry { .. }
            | AgentEvent::ContextTrimmed { .. }
            | AgentEvent::ToolBudgetWarning { .. }
            | AgentEvent::ToolCallApprovalRequest { .. }
            | AgentEvent::ToolCallApprovalDecision { .. }
            | AgentEvent::ToolCallBlocked { .. }
            | AgentEvent::PendingInputQueued { .. }
            | AgentEvent::PendingInputEdited { .. }
            | AgentEvent::PendingInputRemoved { .. }
            | AgentEvent::PendingInputCleared { .. }
            | AgentEvent::PendingInputDequeued { .. }
            | AgentEvent::SubagentStarted { .. }
            | AgentEvent::SubagentCompleted { .. }
            | AgentEvent::SubagentFailed { .. }
            | AgentEvent::FileChange { .. }
            | AgentEvent::TurnDiff { .. }
            | AgentEvent::GitCheckpoint { .. }
            | AgentEvent::CompactionStarted { .. }
            | AgentEvent::CompactionFailed { .. }
            | AgentEvent::HookStarted { .. }
            | AgentEvent::HookCompleted { .. }
            | AgentEvent::SessionRewound { .. } => {
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
    /// Opens the session database. When `path` is `None` the XDG data
    /// directory — `$XDG_DATA_HOME/nav/nav.db`, falling back to
    /// `~/.local/share/nav/nav.db` — is used. Relative overrides are resolved
    /// inside that same nav data directory.
    ///
    /// If SQLite refuses any of the pragmas we set (e.g. WAL is not available
    /// on the underlying filesystem), the mismatch is echoed to stderr so a
    /// misconfiguration is visible at startup. The happy path is silent.
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
        if !journal_mode.eq_ignore_ascii_case("wal") || synchronous != 1 || foreign_keys != 1 {
            eprintln!(
                "nav-core: opened {} with unexpected pragmas (journal_mode={journal_mode}, synchronous={synchronous}, foreign_keys={foreign_keys})",
                path.display()
            );
        }

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

    /// Updates the model selector stored on this session. The model takes
    /// effect the next time the session is resumed (i.e. after restart).
    pub fn set_session_model(&self, session_id: &str, model: &str) -> Result<()> {
        let trimmed = model.trim();
        anyhow::ensure!(!trimmed.is_empty(), "model selector cannot be empty");
        let conn = self.lock();
        let updated = conn.execute(
            "UPDATE session SET model = ?1, updated_at = ?2 WHERE id = ?3",
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
            AgentEvent::ToolCallApprovalDecision {
                approval_id,
                decision,
            } => {
                tx.execute(
                    "UPDATE approval
                     SET decided_at = ?1, decision = ?2
                     WHERE session_id = ?3 AND approval_id = ?4",
                    params![now, decision.as_str(), session_id, approval_id],
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
    pub fn load_session(&self, session_id: &str) -> Result<Vec<AgentEvent>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT seq, data FROM event WHERE session_id = ?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (seq, data) = row?;
            let event = serde_json::from_str::<AgentEvent>(&data).with_context(|| {
                format!("failed to parse event row (session={session_id} seq={seq})")
            })?;
            events.push(event);
        }
        Ok(events)
    }

    /// Latest `TurnComplete.tokens_input` for `session_id`, or `None` if no
    /// turn has completed yet. Under `store: false` this equals current
    /// context-window occupancy (each iteration resends the full history), so
    /// it's the right auto-compaction signal — unlike the cumulative rollup
    /// in `session.tokens_input`, which double-counts the same context.
    pub(crate) fn latest_input_tokens(&self, session_id: &str) -> Result<Option<u64>> {
        let conn = self.lock();
        let row: Option<i64> = conn
            .query_row(
                "SELECT CAST(json_extract(data, '$.usage.tokens_input') AS INTEGER)
                 FROM event
                 WHERE session_id = ?1 AND kind = 'turn_complete'
                 ORDER BY seq DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.map(|n| n.max(0) as u64))
    }

    /// Returns the rolling token totals recorded against `session_id`. The
    /// rolling rollup is for lifetime-spend reporting; auto-compaction uses
    /// [`Self::latest_input_tokens`] instead so the threshold tracks current
    /// context-window occupancy rather than cumulative spend.
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
        let mut stmt = conn.prepare(&summary_query(
            "WHERE ?1 IS NULL OR cwd = ?1 ORDER BY updated_at DESC",
        ))?;
        let rows = stmt.query_map(params![cwd_str], summary_from_row)?;
        let mut summaries = Vec::new();
        for row in rows {
            summaries.push(row?);
        }
        drop(stmt);
        attach_labels(&conn, &mut summaries)?;
        Ok(summaries)
    }

    /// Return the `parent_id` of `session_id` if it exists. `Ok(None)` covers
    /// both "session has no parent" and "session does not exist" — callers
    /// walking ancestors don't usually need to distinguish those.
    pub fn session_parent_id(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.lock();
        let parent: Option<Option<String>> = conn
            .query_row(
                "SELECT parent_id FROM session WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(parent.flatten())
    }

    /// Fetch a single [`SessionSummary`] by canonical ULID. Returns `None`
    /// when the session does not exist.
    pub fn session_summary(&self, session_id: &str) -> Result<Option<SessionSummary>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&summary_query("WHERE id = ?1 LIMIT 1"))?;
        let mut rows = stmt.query_map(params![session_id], summary_from_row)?;
        let summary = match rows.next() {
            Some(row) => Some(row?),
            None => None,
        };
        drop(rows);
        drop(stmt);
        let mut summaries: Vec<SessionSummary> = summary.into_iter().collect();
        attach_labels(&conn, &mut summaries)?;
        Ok(summaries.into_iter().next())
    }

    /// Create a new session that copies events `[0..=at_seq]` from `source_id`
    /// (or every event when `at_seq` is `None`), records `parent_id` +
    /// `fork_point_seq`, and recomputes token totals from the copied turns.
    pub fn fork_session(
        &self,
        source_id: &str,
        at_seq: Option<u64>,
        name: Option<&str>,
    ) -> Result<SessionId> {
        let new_id = Ulid::new().to_string();
        let now = now_secs();
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let parent_meta: Option<(String, String, String, Option<String>)> = tx
            .query_row(
                "SELECT cwd, provider, model, profile FROM session WHERE id = ?1",
                params![source_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let (cwd, provider, model, profile) =
            parent_meta.ok_or_else(|| anyhow::anyhow!("session not found: {source_id}"))?;

        let max_seq: Option<i64> = tx
            .query_row(
                "SELECT MAX(seq) FROM event WHERE session_id = ?1",
                params![source_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();

        // Resolve the actual fork-point seq from the requested upper bound.
        // - explicit `at_seq` is clamped to the highest existing seq;
        // - `None` means "fork at now" → the highest existing seq;
        // - a parent with no events stays at NULL (root-equivalent fork).
        let fork_seq: Option<i64> = match (at_seq, max_seq) {
            (_, None) => None,
            (None, Some(max)) => Some(max),
            (Some(req), Some(max)) => Some((req as i64).min(max)),
        };

        tx.execute(
            "INSERT INTO session (
                id, cwd, provider, model, profile, name,
                created_at, updated_at, parent_id, fork_point_seq
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
            params![
                new_id, cwd, provider, model, profile, name, now, source_id, fork_seq
            ],
        )?;

        if let Some(end) = fork_seq {
            tx.execute(
                "INSERT INTO event (session_id, seq, created_at, kind, data)
                 SELECT ?1, seq, created_at, kind, data
                 FROM event
                 WHERE session_id = ?2 AND seq <= ?3",
                params![new_id, source_id, end],
            )?;

            // Recompute the new session's rolling token totals by replaying
            // each copied turn_complete payload. Selecting the JSON usage
            // straight out of the event log avoids re-running the agent
            // loop and stays consistent with append_event's roll-ups.
            let (sum_in, sum_out, sum_in_cached, sum_reason): (i64, i64, i64, i64) = tx
                .query_row(
                    "SELECT
                         COALESCE(SUM(CAST(json_extract(data, '$.usage.tokens_input') AS INTEGER)), 0),
                         COALESCE(SUM(CAST(json_extract(data, '$.usage.tokens_output') AS INTEGER)), 0),
                         COALESCE(SUM(CAST(json_extract(data, '$.usage.tokens_input_cached') AS INTEGER)), 0),
                         COALESCE(SUM(CAST(json_extract(data, '$.usage.tokens_reasoning') AS INTEGER)), 0)
                     FROM event
                     WHERE session_id = ?1 AND kind = 'turn_complete'",
                    params![new_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
            tx.execute(
                "UPDATE session
                 SET tokens_input = ?1,
                     tokens_output = ?2,
                     tokens_input_cached = ?3,
                     tokens_reasoning = ?4
                 WHERE id = ?5",
                params![sum_in, sum_out, sum_in_cached, sum_reason, new_id],
            )?;
        }

        tx.commit()?;
        Ok(new_id)
    }

    /// In-session counterpart to [`Self::fork_session`]. Removes the
    /// `user_message` at `target_seq` and every later event from the session's
    /// own log, appends a [`AgentEvent::SessionRewound`] audit row in their
    /// place, and recomputes the rolling token totals from the surviving
    /// `TurnComplete` events. The returned [`RewindOutcome`] carries the
    /// original message fields so callers (the TUI composer, the headless
    /// CLI) can repopulate the prompt for editing before the next turn.
    ///
    /// Errors when the row at `(session_id, target_seq)` is missing or is
    /// not a `user_message` — both signal a stale UI seq or an invalid CLI
    /// argument, and silently choosing a different anchor would corrupt the
    /// transcript.
    pub fn rewind_to_user_message(
        &self,
        session_id: &str,
        target_seq: u64,
    ) -> Result<RewindOutcome> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        let target_signed = target_seq as i64;
        let target_row: Option<(String, String)> = tx
            .query_row(
                "SELECT kind, data FROM event WHERE session_id = ?1 AND seq = ?2",
                params![session_id, target_signed],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let (kind, data) = target_row.ok_or_else(|| {
            anyhow::anyhow!("session {session_id} has no event at seq {target_seq}")
        })?;
        anyhow::ensure!(
            kind == "user_message",
            "event at seq {target_seq} is kind {kind:?}, not user_message"
        );

        let event: AgentEvent = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse user_message at seq {target_seq}"))?;
        let AgentEvent::UserMessage {
            text,
            display_text,
            attachments,
        } = event
        else {
            anyhow::bail!("event at seq {target_seq} deserialized to a non-user_message variant");
        };

        let removed_events = tx.execute(
            "DELETE FROM event WHERE session_id = ?1 AND seq >= ?2",
            params![session_id, target_signed],
        )?;

        let audit_event = AgentEvent::SessionRewound {
            target_seq,
            removed_events,
            preview: preview_for_audit(display_text.as_deref().unwrap_or(&text)),
        };
        let audit_data =
            serde_json::to_string(&audit_event).context("failed to serialize rewind event")?;
        let now = now_secs();
        // The previous DELETE freed `target_seq`, so the next-seq seed lands
        // exactly there. Keeping the audit row at the same seq the original
        // user_message occupied means downstream cursors that referenced the
        // anchor still resolve to a recognisable position.
        tx.execute(
            "INSERT INTO event (session_id, seq, created_at, kind, data)
             VALUES (
                 ?1,
                 COALESCE((SELECT MAX(seq) FROM event WHERE session_id = ?1), -1) + 1,
                 ?2, ?3, ?4
             )",
            params![session_id, now, audit_event.kind(), audit_data],
        )?;

        // Recompute rolling token totals from the surviving `TurnComplete`
        // events so the next auto-compaction decision uses the trimmed
        // baseline instead of the pre-rewind one.
        let (sum_in, sum_out, sum_in_cached, sum_reason): (i64, i64, i64, i64) = tx.query_row(
            "SELECT
                 COALESCE(SUM(CAST(json_extract(data, '$.usage.tokens_input') AS INTEGER)), 0),
                 COALESCE(SUM(CAST(json_extract(data, '$.usage.tokens_output') AS INTEGER)), 0),
                 COALESCE(SUM(CAST(json_extract(data, '$.usage.tokens_input_cached') AS INTEGER)), 0),
                 COALESCE(SUM(CAST(json_extract(data, '$.usage.tokens_reasoning') AS INTEGER)), 0)
             FROM event
             WHERE session_id = ?1 AND kind = 'turn_complete'",
            params![session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;

        // Trim the `turn` table and recompute session-level turn rollups so
        // `/sessions`, tree summaries, and future `turn_index` calculations
        // stop counting the rewound-past turns. The surviving turn rows are
        // the first N where N equals the count of surviving `turn_complete`
        // events — turn rows are created 1:1 with that event in
        // `complete_turn`, in insertion (i.e. seq) order.
        let surviving_turn_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM event WHERE session_id = ?1 AND kind = 'turn_complete'",
            params![session_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "DELETE FROM turn WHERE session_id = ?1 AND turn_index >= ?2",
            params![session_id, surviving_turn_count],
        )?;
        let (reported_cost_sum, reported_cost_count): (i64, i64) = tx.query_row(
            "SELECT
                 COALESCE(SUM(cost_micros), 0),
                 COALESCE(SUM(CASE WHEN cost_source = ?1 THEN 1 ELSE 0 END), 0)
             FROM turn
             WHERE session_id = ?2 AND cost_source = ?1",
            params![COST_SOURCE_REPORTED, session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        tx.execute(
            "UPDATE session
             SET tokens_input = ?1,
                 tokens_output = ?2,
                 tokens_input_cached = ?3,
                 tokens_reasoning = ?4,
                 turns_total = ?5,
                 turns_with_reported_cost = ?6,
                 cost_micros_reported = ?7,
                 updated_at = ?8
             WHERE id = ?9",
            params![
                sum_in,
                sum_out,
                sum_in_cached,
                sum_reason,
                surviving_turn_count,
                reported_cost_count,
                reported_cost_sum,
                now,
                session_id,
            ],
        )?;

        tx.commit()?;
        Ok(RewindOutcome {
            target_seq,
            removed_events,
            text,
            display_text,
            attachments,
        })
    }

    /// Returns the highest `user_message` seq recorded for `session_id`, or
    /// `None` when the session has not yet captured one. Used by the TUI's
    /// `/rewind` (no-arg form) to default to "edit the most recent submitted
    /// prompt" without forcing the user to look up a seq.
    pub fn latest_user_message_seq(&self, session_id: &str) -> Result<Option<u64>> {
        let conn = self.lock();
        let row: Option<i64> = conn
            .query_row(
                "SELECT MAX(seq) FROM event
                 WHERE session_id = ?1 AND kind = 'user_message'",
                params![session_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        Ok(row.map(|seq| seq as u64))
    }

    /// Direct children of `parent_id`, ordered by creation time ascending.
    pub fn list_children(&self, parent_id: &str) -> Result<Vec<SessionSummary>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&summary_query(
            "WHERE parent_id = ?1 ORDER BY created_at ASC",
        ))?;
        let rows = stmt.query_map(params![parent_id], summary_from_row)?;
        let mut summaries: Vec<SessionSummary> = rows.collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        attach_labels(&conn, &mut summaries)?;
        Ok(summaries)
    }

    /// Flat depth-ordered list of every descendant of `root_id`, including
    /// the root itself at depth 0. Used by `/tree` and `nav sessions tree`.
    ///
    /// One recursive CTE collects `(id, depth)` for the whole subtree, then a
    /// single join against the summary projection rehydrates each row; one
    /// batched `attach_labels` populates labels in one more query. That keeps
    /// the cost flat at three prepared statements regardless of tree size.
    pub fn walk_tree(&self, root_id: &str) -> Result<Vec<SessionTreeNode>> {
        let conn = self.lock();
        let sql = format!(
            "WITH RECURSIVE tree(id, depth) AS (
                 SELECT id, 0 FROM session WHERE id = ?1
                 UNION ALL
                 SELECT s.id, tree.depth + 1
                 FROM session AS s
                 JOIN tree ON s.parent_id = tree.id
             )
             SELECT {SESSION_SUMMARY_COLUMNS}, tree.depth AS tree_depth
             FROM session
             JOIN tree ON tree.id = session.id
             ORDER BY tree.depth ASC, session.created_at ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        // The CTE appends `tree_depth` after the 18 summary columns.
        let rows = stmt.query_map(params![root_id], |row| {
            let summary = summary_from_row(row)?;
            let depth: i64 = row.get(18)?;
            Ok(SessionTreeNode {
                summary,
                depth: depth.max(0) as u32,
            })
        })?;
        let mut out: Vec<SessionTreeNode> = rows.collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        if out.is_empty() {
            anyhow::bail!("session not found: {root_id}");
        }
        let mut summaries: Vec<SessionSummary> =
            out.iter().map(|node| node.summary.clone()).collect();
        attach_labels(&conn, &mut summaries)?;
        for (node, summary) in out.iter_mut().zip(summaries) {
            node.summary = summary;
        }
        Ok(out)
    }

    /// Attach `label` to `session_id`. No-ops if the row already exists.
    pub fn add_label(&self, session_id: &str, label: &str) -> Result<()> {
        let trimmed = label.trim();
        anyhow::ensure!(!trimmed.is_empty(), "label cannot be empty");
        let conn = self.lock();
        conn.execute(
            "INSERT OR IGNORE INTO label (session_id, label, created_at)
             VALUES (?1, ?2, ?3)",
            params![session_id, trimmed, now_secs()],
        )?;
        Ok(())
    }

    /// Detach `label` from `session_id`. Silent when the label was not set.
    pub fn remove_label(&self, session_id: &str, label: &str) -> Result<()> {
        let trimmed = label.trim();
        anyhow::ensure!(!trimmed.is_empty(), "label cannot be empty");
        let conn = self.lock();
        conn.execute(
            "DELETE FROM label WHERE session_id = ?1 AND label = ?2",
            params![session_id, trimmed],
        )?;
        Ok(())
    }

    /// All labels currently attached to `session_id`, sorted alphabetically.
    pub fn labels_for(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT label FROM label WHERE session_id = ?1 ORDER BY label ASC")?;
        let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        let mut labels = Vec::new();
        for row in rows {
            labels.push(row?);
        }
        Ok(labels)
    }

    /// All sessions carrying `label`, newest first. Pair with `cwd` to scope
    /// the listing to one workspace (mirrors `list_sessions`).
    pub fn list_by_label(&self, label: &str, cwd: Option<&Path>) -> Result<Vec<SessionSummary>> {
        let conn = self.lock();
        let cwd_str: Option<String> = cwd.map(|p| p.to_string_lossy().into_owned());
        let mut stmt = conn.prepare(&summary_query(
            "WHERE id IN (SELECT session_id FROM label WHERE label = ?1)
               AND (?2 IS NULL OR cwd = ?2)
             ORDER BY updated_at DESC",
        ))?;
        let rows = stmt.query_map(params![label, cwd_str], summary_from_row)?;
        let mut summaries: Vec<SessionSummary> = rows.collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        attach_labels(&conn, &mut summaries)?;
        Ok(summaries)
    }

    /// Run an FTS5 MATCH against the persisted user/assistant transcripts and
    /// return up to `limit` hits, newest session first. `query` is passed
    /// straight to FTS5 — callers that need to support arbitrary user input
    /// (with quotes, OR, etc.) should hand the raw string in unchanged.
    pub fn search_transcript(
        &self,
        query: &str,
        limit: usize,
        label: Option<&str>,
    ) -> Result<Vec<TranscriptHit>> {
        let trimmed = query.trim();
        anyhow::ensure!(!trimmed.is_empty(), "search query cannot be empty");
        let conn = self.lock();
        let cap = (limit as i64).max(1);
        let label_opt: Option<&str> = label;
        // `?2 IS NULL OR session_id IN (...)` folds the unfiltered and
        // label-filtered queries into one prepared statement; SQLite
        // short-circuits the disjunction so the label index is still used
        // when a label is supplied.
        let mut stmt = conn.prepare(
            "SELECT session_id, seq, kind,
                    snippet(event_fts, 3, '[', ']', '…', 16) AS snippet
             FROM event_fts
             WHERE event_fts MATCH ?1
               AND (?2 IS NULL
                    OR session_id IN (SELECT session_id FROM label WHERE label = ?2))
             ORDER BY rank, session_id, seq
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![trimmed, label_opt, cap], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (session_id, seq, kind, snippet) = row?;
            hits.push((session_id, seq, kind, snippet));
        }
        drop(stmt);
        drop(conn);
        let mut summary_cache: HashMap<String, SessionSummary> = HashMap::new();
        let mut out = Vec::with_capacity(hits.len());
        for (session_id, seq, kind, snippet) in hits {
            let summary = match summary_cache.get(&session_id) {
                Some(existing) => existing.clone(),
                None => {
                    let Some(loaded) = self.session_summary(&session_id)? else {
                        continue;
                    };
                    summary_cache.insert(session_id.clone(), loaded.clone());
                    loaded
                }
            };
            out.push(TranscriptHit {
                session_id,
                seq,
                kind,
                snippet,
                summary,
            });
        }
        Ok(out)
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
        // Escape SQLite LIKE wildcards (`%`, `_`) and the escape character
        // itself in the user/model-supplied prefix. Session IDs are ULIDs
        // (Crockford base32) which never contain these characters, so this
        // is a no-op for valid prefixes — but it defangs a prompt-injected
        // call into `read_thread` that would otherwise pass `%` and match
        // arbitrary stored sessions.
        let escaped_prefix = escape_like_pattern(prefix);
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM session WHERE id LIKE ?1 ESCAPE '\\' ORDER BY id ASC LIMIT 3")
            .map_err(|_| not_found())?;
        let rows = stmt
            .query_map(params![format!("{escaped_prefix}%")], |row| {
                row.get::<_, String>(0)
            })
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

/// Resolves the on-disk location of the session database without opening it.
/// `nav doctor` calls this to surface the resolved path even when the DB
/// file does not exist yet — the answer should be identical to what
/// [`SessionStore::open`] would compute internally.
pub fn resolved_db_path(path: Option<PathBuf>) -> Result<PathBuf> {
    resolve_db_path(path)
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

pub(crate) fn xdg_data_home() -> Option<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("share")))
}

fn apply_schema(conn: &Connection) -> Result<()> {
    if schema_is_stale(conn)? {
        reset_schema(conn)?;
    }
    conn.execute_batch(INIT_SQL)
        .context("failed to apply nav-core schema")?;
    record_schema_version(conn, SCHEMA_VERSION)?;
    Ok(())
}

fn schema_is_stale(conn: &Connection) -> Result<bool> {
    if !table_exists(conn, "schema_version")? {
        return table_exists(conn, "session");
    }
    let version = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .optional()?
        .flatten();
    Ok(version != Some(SCHEMA_VERSION))
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn reset_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS event_fts_ai;
         DROP TRIGGER IF EXISTS event_fts_ad;
         DROP TABLE IF EXISTS event_fts;
         DROP TABLE IF EXISTS label;
         DROP TABLE IF EXISTS approval;
         DROP TABLE IF EXISTS turn;
         DROP TABLE IF EXISTS event;
         DROP TABLE IF EXISTS session;
         DROP TABLE IF EXISTS schema_version;",
    )
    .context("failed to reset stale nav session schema")?;
    Ok(())
}

fn record_schema_version(conn: &Connection, version: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (?1, ?2)",
        params![version, now_secs()],
    )?;
    Ok(())
}

/// Shared column list used by every `SessionSummary` query. The column order
/// must stay in sync with [`summary_from_row`] (which reads by index).
/// Every column is qualified with `session.` so the list is safe to embed
/// inside a JOIN that introduces another table with overlapping column names
/// (e.g. the `tree(id, depth)` CTE in [`walk_tree`]).
const SESSION_SUMMARY_COLUMNS: &str =
    "session.id, session.name, session.created_at, session.updated_at, session.cwd,
     session.provider, session.model,
     session.tokens_input, session.tokens_output, session.tokens_input_cached,
     session.tokens_reasoning,
     session.cost_micros_reported, session.turns_with_reported_cost, session.turns_total,
     session.cost_currency,
     (
         SELECT data FROM event
         WHERE event.session_id = session.id AND kind = 'user_message'
         ORDER BY seq ASC
         LIMIT 1
     ) AS first_user_event,
     session.parent_id,
     (SELECT COUNT(*) FROM session AS child WHERE child.parent_id = session.id) AS child_count";

/// `SELECT <SESSION_SUMMARY_COLUMNS> FROM session ` (trailing space included
/// so callers can append `WHERE …` directly).
fn summary_query(suffix: &str) -> String {
    let prefix_len = "SELECT  FROM session ".len() + SESSION_SUMMARY_COLUMNS.len();
    let mut sql = String::with_capacity(prefix_len + suffix.len());
    sql.push_str("SELECT ");
    sql.push_str(SESSION_SUMMARY_COLUMNS);
    sql.push_str(" FROM session ");
    sql.push_str(suffix);
    sql
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummary> {
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
        parent_id: row.get(16)?,
        labels: Vec::new(),
        child_count: row.get::<_, i64>(17)? as u64,
    })
}

/// Populate [`SessionSummary::labels`] in one batched query rather than firing
/// `labels_for` per row. Caller owns the conn lock.
fn attach_labels(conn: &Connection, summaries: &mut [SessionSummary]) -> Result<()> {
    if summaries.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = summaries.iter().map(|s| s.id.clone()).collect();
    // SQLite's IN clause does not accept a bound parameter array, so we build
    // a list of ?N placeholders matching the session count. This stays well
    // under the default 999-parameter limit for any plausible listing.
    let placeholders: String = (0..ids.len())
        .map(|i| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT session_id, label FROM label
         WHERE session_id IN ({placeholders})
         ORDER BY label ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let params_iter: Vec<&dyn rusqlite::ToSql> =
        ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(params_iter.as_slice(), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut by_id: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let (sid, label) = row?;
        by_id.entry(sid).or_default().push(label);
    }
    for summary in summaries.iter_mut() {
        if let Some(labels) = by_id.remove(&summary.id) {
            summary.labels = labels;
        }
    }
    Ok(())
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

/// First line of `text`, truncated to a short audit-friendly slice. The
/// preview is stored on [`AgentEvent::SessionRewound`] so the durable log
/// records which message was rewound past without copying the full body —
/// the original event is gone from the log by then, and a tiny excerpt is
/// usually enough to reconstruct intent during review.
const REWIND_PREVIEW_MAX_CHARS: usize = 120;
fn preview_for_audit(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= REWIND_PREVIEW_MAX_CHARS {
        return first_line.to_string();
    }
    let mut out: String = first_line.chars().take(REWIND_PREVIEW_MAX_CHARS).collect();
    out.push('…');
    out
}

/// Escape SQLite `LIKE` metacharacters in `prefix` using `\` as the escape
/// character. Use together with `ESCAPE '\\'` on the LIKE clause so a
/// caller-supplied prefix containing `%` or `_` matches literally rather
/// than as a wildcard.
fn escape_like_pattern(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len());
    for ch in prefix.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// [`DurableEventSink`] adaptor that writes through a [`SessionStore`].
///
/// The `ChannelGate` lives outside `run_agent`'s emit path, so without this
/// the approval-request event would only land on the live `events` channel
/// and never reach SQLite, leaving the later approval decision with no audit
/// row to update. Build one with [`SessionStore::sink_for`].
pub struct SessionStoreSink {
    store: std::sync::Arc<SessionStore>,
    session_id: String,
}

impl crate::guardrails::approval::DurableEventSink for SessionStoreSink {
    fn persist(&self, event: &AgentEvent) {
        if let Err(err) = self.store.append_event(&self.session_id, event) {
            // Persistence is best-effort: a SQLite hiccup must not stall
            // the live conversation. Log once and continue.
            eprintln!("nav-core: failed to persist approval event: {err:#}");
        }
    }
}

impl crate::guardrails::approval::DecisionRecorder for SessionStoreSink {
    fn record(&self, approval_id: &str, decision: crate::guardrails::ReviewDecision) {
        let event = AgentEvent::ToolCallApprovalDecision {
            approval_id: approval_id.to_string(),
            decision,
        };
        if let Err(err) = self.store.append_event(&self.session_id, &event) {
            eprintln!("nav-core: failed to persist approval decision: {err:#}");
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

mod reference;
#[cfg(test)]
mod tests;
