//! SQLite-backed session storage: connection setup and write concurrency.
//!
//! This module owns the low-level database concerns shared by every higher
//! layer: opening the connection with the agreed pragmas, falling back from
//! WAL to a rollback journal on network filesystems, and serialising writes
//! through `BEGIN IMMEDIATE` with jittered retry to avoid convoy effects.
//! Schema and row persistence land in follow-up issues.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::migrate;

/// Maximum number of `BEGIN IMMEDIATE` retries before giving up on a busy DB.
const MAX_WRITE_RETRIES: u32 = 15;
/// Retry backoff is uniform jitter in `[JITTER_MIN_MS, JITTER_MAX_MS]`.
const JITTER_MIN_MS: u64 = 20;
const JITTER_MAX_MS: u64 = 150;
/// Run `wal_checkpoint(PASSIVE)` once every this many committed writes.
const CHECKPOINT_INTERVAL: u64 = 50;

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
    journal_mode: JournalMode,
    writes: AtomicU64,
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
    /// Schema migration or reconciliation failed.
    MigrationFailed(String),
}

impl std::fmt::Display for SqliteStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenFailed(msg) => write!(f, "open failed: {msg}"),
            Self::PragmaFailed(msg) => write!(f, "pragma failed: {msg}"),
            Self::Locking(msg) => write!(f, "locking protocol error: {msg}"),
            Self::WriteFailed(msg) => write!(f, "write failed: {msg}"),
            Self::MigrationFailed(msg) => write!(f, "migration failed: {msg}"),
        }
    }
}

impl std::error::Error for SqliteStoreError {}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert_index_exists(&conn, "idx_runs_session_started");
        assert_index_exists(&conn, "idx_turns_run_seq");
        assert_index_exists(&conn, "idx_turn_parts_turn_id");
        assert_index_exists(&conn, "idx_turn_parts_session_id");

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
    fn core_schema_accepts_rows_without_provider_payloads_table() {
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
