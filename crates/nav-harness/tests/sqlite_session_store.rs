use std::path::{Path, PathBuf};

use nav_harness::sessions::{
    CreateSession, RunStatus, SessionSettings, SqliteSessionStore, SqliteStoreError, StartRun,
    TokenDelta,
};
use nav_types::{RunId, SessionId};

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "nav-sqlite-session-store-{name}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = self.path.clone().into_os_string();
            name.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(name));
        }
    }
}

fn session_id(value: &str) -> SessionId {
    SessionId::new_unchecked(value)
}

fn run_id(value: &str) -> RunId {
    RunId::new_unchecked(value)
}

fn create_minimal_session(store: &SqliteSessionStore, session_id: SessionId) {
    store
        .create_session(
            session_id,
            CreateSession {
                title: None,
                source: "cli".to_string(),
                workspace_root: None,
                system_prompt: None,
                settings_json: "{}".to_string(),
                parent_id: None,
                version: "test-version".to_string(),
                slug: None,
                created_at: 1_000,
            },
        )
        .expect("session create should commit");
}

#[test]
fn sessions_can_be_created_read_and_settings_updated() {
    let db = TempDb::new("session-crud");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000332");

    store
        .create_session(
            session_id.clone(),
            CreateSession {
                title: Some("Storage slice".to_string()),
                source: "tui".to_string(),
                workspace_root: Some("/tmp/nav-workspace".to_string()),
                system_prompt: Some("Build carefully.".to_string()),
                settings_json: r#"{"model":"old"}"#.to_string(),
                parent_id: None,
                version: "test-version".to_string(),
                slug: Some("storage-slice".to_string()),
                created_at: 1_000,
            },
        )
        .expect("session create should commit");

    let created = store
        .get_session(&session_id)
        .expect("created session should be readable");
    assert_eq!(created.id, session_id);
    assert_eq!(created.title.as_deref(), Some("Storage slice"));
    assert_eq!(created.source, "tui");
    assert_eq!(
        created.workspace_root.as_deref(),
        Some("/tmp/nav-workspace")
    );
    assert_eq!(created.system_prompt.as_deref(), Some("Build carefully."));
    assert_eq!(created.settings_json, r#"{"model":"old"}"#);
    assert_eq!(created.version, "test-version");
    assert_eq!(created.slug.as_deref(), Some("storage-slice"));
    assert_eq!(created.cost, 0.0);
    assert_eq!(created.tokens_input, 0);
    assert_eq!(created.created_at, 1_000);
    assert_eq!(created.updated_at, 1_000);

    store
        .update_session_settings(
            &session_id,
            SessionSettings {
                settings_json: r#"{"model":"new"}"#.to_string(),
                updated_at: 1_500,
            },
        )
        .expect("settings update should commit");

    let updated = store
        .get_session(&session_id)
        .expect("updated session should be readable");
    assert_eq!(updated.settings_json, r#"{"model":"new"}"#);
    assert_eq!(updated.created_at, 1_000);
    assert_eq!(updated.updated_at, 1_500);
}

#[test]
fn runs_can_be_started_and_finished() {
    let db = TempDb::new("run-crud");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000333");
    let run_id = run_id("019e7000-0000-7000-8000-000000000334");
    create_minimal_session(&store, session_id.clone());

    store
        .start_run(StartRun {
            id: run_id.clone(),
            session_id: session_id.clone(),
            status: RunStatus::Pending,
            trigger: Some("user".to_string()),
            started_at: 2_000,
        })
        .expect("run start should commit");

    let started = store.get_run(&run_id).expect("run should be readable");
    assert_eq!(started.id, run_id);
    assert_eq!(started.session_id, session_id);
    assert_eq!(started.status, "pending");
    assert_eq!(started.trigger.as_deref(), Some("user"));
    assert_eq!(started.started_at, 2_000);
    assert_eq!(started.finished_at, None);
    assert_eq!(started.error_json, None);

    store
        .finish_run(&run_id, RunStatus::Completed, 2_500, None)
        .expect("run finish should commit");

    let finished = store.get_run(&run_id).expect("run should remain readable");
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.finished_at, Some(2_500));
    assert_eq!(finished.error_json, None);
}

#[test]
fn terminal_runs_cannot_be_finished_again() {
    let db = TempDb::new("run-terminal");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000337");
    let run_id = run_id("019e7000-0000-7000-8000-000000000338");
    create_minimal_session(&store, session_id.clone());

    store
        .start_run(StartRun {
            id: run_id.clone(),
            session_id,
            status: RunStatus::Running,
            trigger: Some("user".to_string()),
            started_at: 2_000,
        })
        .expect("run start should commit");
    store
        .finish_run(&run_id, RunStatus::Cancelled, 2_100, None)
        .expect("first terminal transition should commit");

    let err = store
        .finish_run(&run_id, RunStatus::Completed, 2_500, None)
        .expect_err("terminal run should reject a second finish");
    assert!(matches!(err, SqliteStoreError::InvalidRunTransition { .. }));

    let finished = store.get_run(&run_id).expect("run should remain readable");
    assert_eq!(finished.status, "cancelled");
    assert_eq!(finished.finished_at, Some(2_100));
}

#[test]
fn cost_deltas_can_be_reversed_without_changing_the_session_row() {
    let db = TempDb::new("cost-reversal");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000335");
    create_minimal_session(&store, session_id.clone());
    let before = store
        .get_session(&session_id)
        .expect("session should be readable before cost updates");
    let delta = TokenDelta {
        input: 10,
        output: 20,
        reasoning: 30,
        cache_read: 40,
        cache_write: 50,
    };

    store
        .update_session_cost(&session_id, 0.125, delta)
        .expect("cost update should commit");

    let charged = store
        .get_session(&session_id)
        .expect("session should be readable after cost update");
    assert_eq!(charged.cost, 0.125);
    assert_eq!(charged.tokens_input, 10);
    assert_eq!(charged.tokens_output, 20);
    assert_eq!(charged.tokens_reasoning, 30);
    assert_eq!(charged.tokens_cache_read, 40);
    assert_eq!(charged.tokens_cache_write, 50);

    store
        .reverse_session_cost(&session_id, 0.125, delta)
        .expect("cost reversal should commit");

    let reversed = store
        .get_session(&session_id)
        .expect("session should be readable after cost reversal");
    assert_eq!(reversed, before);
}

#[test]
fn concurrent_cost_writers_preserve_every_delta() {
    const WRITERS: usize = 6;
    const WRITES_EACH: usize = 12;

    let db = TempDb::new("cost-concurrent");
    let path = db.path().to_path_buf();
    let session_id = session_id("019e7000-0000-7000-8000-000000000336");
    let setup = SqliteSessionStore::open(&path).expect("setup open should succeed");
    create_minimal_session(&setup, session_id.clone());
    drop(setup);

    let writer_stores: Vec<_> = (0..WRITERS)
        .map(|_| SqliteSessionStore::open(&path).expect("writer open should succeed"))
        .collect();

    let handles: Vec<_> = writer_stores
        .into_iter()
        .map(|store| {
            let session_id = session_id.clone();
            std::thread::spawn(move || {
                for _ in 0..WRITES_EACH {
                    store
                        .update_session_cost(
                            &session_id,
                            0.5,
                            TokenDelta {
                                input: 1,
                                output: 2,
                                reasoning: 3,
                                cache_read: 4,
                                cache_write: 5,
                            },
                        )
                        .expect("cost update should survive writer contention");
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("writer thread should not panic");
    }

    let reader = SqliteSessionStore::open(&path).expect("reader open should succeed");
    let session = reader
        .get_session(&session_id)
        .expect("session should be readable after concurrent updates");
    let total_writes = (WRITERS * WRITES_EACH) as i64;
    assert_eq!(session.cost, total_writes as f64 * 0.5);
    assert_eq!(session.tokens_input, total_writes);
    assert_eq!(session.tokens_output, total_writes * 2);
    assert_eq!(session.tokens_reasoning, total_writes * 3);
    assert_eq!(session.tokens_cache_read, total_writes * 4);
    assert_eq!(session.tokens_cache_write, total_writes * 5);
}
