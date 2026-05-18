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
pub const SCHEMA_VERSION: i64 = 1;

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

/// One row returned by [`SessionStore::list_sessions`], with the rollup fields
/// the CLI's `--list-sessions` command formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub updated_at: i64,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_input_cached: u64,
    pub tokens_reasoning: u64,
    pub cost_micros_reported: i64,
    pub turns_with_reported_cost: u64,
    pub turns_total: u64,
    pub cost_currency: String,
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
        let id = Ulid::new().to_string();
        let now = now_secs();
        let cwd_str = cwd.to_string_lossy().into_owned();
        let conn = self.lock();
        conn.execute(
            "INSERT INTO session (id, cwd, provider, model, profile, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![id, cwd_str, provider, model, profile, now],
        )?;
        Ok(id)
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
        tx.commit()?;
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
    pub fn load_session(&self, session_id: &str) -> Result<Vec<AgentEvent>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT data FROM event WHERE session_id = ?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        let mut events = Vec::new();
        for row in rows {
            let data = row?;
            let event: AgentEvent =
                serde_json::from_str(&data).context("failed to deserialize stored event")?;
            events.push(event);
        }
        Ok(events)
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
            "SELECT id, updated_at, cwd, provider, model,
                    tokens_input, tokens_output, tokens_input_cached, tokens_reasoning,
                    cost_micros_reported, turns_with_reported_cost, turns_total, cost_currency
             FROM session
             WHERE ?1 IS NULL OR cwd = ?1
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(params![cwd_str], |row| {
            Ok(SessionSummary {
                id: row.get(0)?,
                updated_at: row.get(1)?,
                cwd: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                tokens_input: row.get::<_, i64>(5)? as u64,
                tokens_output: row.get::<_, i64>(6)? as u64,
                tokens_input_cached: row.get::<_, i64>(7)? as u64,
                tokens_reasoning: row.get::<_, i64>(8)? as u64,
                cost_micros_reported: row.get(9)?,
                turns_with_reported_cost: row.get::<_, i64>(10)? as u64,
                turns_total: row.get::<_, i64>(11)? as u64,
                cost_currency: row.get(12)?,
            })
        })?;
        let mut summaries = Vec::new();
        for row in rows {
            summaries.push(row?);
        }
        Ok(summaries)
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
    let already_applied: Option<i64> = conn
        .query_row(
            "SELECT version FROM schema_version WHERE version = ?1",
            params![SCHEMA_VERSION],
            |row| row.get(0),
        )
        .optional()?;
    if already_applied.is_none() {
        conn.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
            params![SCHEMA_VERSION, now_secs()],
        )?;
    }
    Ok(())
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
