//! Durable session storage: a fresh database is created from the canonical
//! schema, and a full exchange is persisted into the shared table shapes.

use nav::{ChatMessage, Role, Storage};
use rusqlite::Connection;

/// A throwaway database path under the OS temp dir.
fn temp_db() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("nav_storage_test_{}.db", uuid::Uuid::now_v7()))
}

struct TempDb(std::path::PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

#[test]
fn opening_a_fresh_database_applies_the_schema_at_version_1() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());

    Storage::open(&path).expect("open fresh database");

    let conn = Connection::open(&path).expect("reopen for assertions");
    let version: i64 = conn
        .query_row("SELECT max(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("schema_migrations is populated");
    assert_eq!(version, 1);

    // Core tables and the FTS virtual table exist.
    for table in ["sessions", "runs", "turns", "turn_parts", "turn_parts_fts"] {
        let found: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "expected table {table} to exist");
    }
}

#[test]
fn a_full_exchange_is_persisted_into_sessions_runs_turns_and_parts() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());
    let storage = Storage::open(&path).expect("open database");

    let session_id = "test-session-1";
    let run_id = "test-run-1";
    storage.create_session(session_id, "nav").unwrap();
    storage.start_run(run_id, session_id).unwrap();
    storage
        .record_user_text(session_id, run_id, 0, "my name is Ada")
        .unwrap();
    storage
        .record_assistant_text(session_id, run_id, 1, "Hi Ada!", Some("qwen-test"))
        .unwrap();
    storage.complete_run(run_id).unwrap();

    let conn = Connection::open(&path).expect("reopen for assertions");

    // Run is completed.
    let status: String = conn
        .query_row("SELECT status FROM runs WHERE id = ?1", [run_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "completed");

    // Two turns: user seq 0, assistant seq 1 carrying the model id.
    let (assistant_role, assistant_model): (String, Option<String>) = conn
        .query_row(
            "SELECT role, model_id FROM turns WHERE run_id = ?1 AND seq = 1",
            [run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(assistant_role, "assistant");
    assert_eq!(assistant_model.as_deref(), Some("qwen-test"));

    // The text parts were mirrored into the FTS index by the schema triggers.
    let hits: i64 = conn
        .query_row(
            "SELECT count(*) FROM turn_parts_fts WHERE turn_parts_fts MATCH 'Ada'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        hits >= 1,
        "the persisted text should be full-text searchable"
    );
}

#[test]
fn load_history_replays_a_session_in_order_for_resume() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());
    let storage = Storage::open(&path).expect("open database");

    let session_id = "resume-session";
    storage.create_session(session_id, "nav").unwrap();

    storage.start_run("run-a", session_id).unwrap();
    storage
        .record_user_text(session_id, "run-a", 0, "first question")
        .unwrap();
    storage
        .record_assistant_text(session_id, "run-a", 1, "first answer", Some("m"))
        .unwrap();
    storage.complete_run("run-a").unwrap();

    storage.start_run("run-b", session_id).unwrap();
    storage
        .record_user_text(session_id, "run-b", 0, "second question")
        .unwrap();

    assert!(storage.session_exists(session_id).unwrap());
    assert!(!storage.session_exists("nope").unwrap());

    let history = storage.load_history(session_id).unwrap();
    assert_eq!(
        history,
        vec![
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("second question"),
        ]
    );
    // Sanity on roles to guard the user/assistant mapping.
    assert_eq!(history[1].role, Role::Assistant);
}

#[test]
fn a_failed_run_records_the_error() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());
    let storage = Storage::open(&path).expect("open database");

    storage.create_session("s", "nav").unwrap();
    storage.start_run("r", "s").unwrap();
    storage.record_user_text("s", "r", 0, "hello").unwrap();
    storage.fail_run("r", "provider exploded").unwrap();

    let conn = Connection::open(&path).expect("reopen for assertions");
    let (status, error_json): (String, Option<String>) = conn
        .query_row(
            "SELECT status, error_json FROM runs WHERE id = 'r'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "failed");
    assert!(error_json.unwrap().contains("provider exploded"));
}

#[test]
fn reopening_an_existing_database_is_a_no_op() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());

    let first = Storage::open(&path).expect("first open");
    first.create_session("keep-me", "nav").unwrap();
    drop(first);

    // Reopening must not re-apply the schema or wipe data.
    let second = Storage::open(&path).expect("reopen");
    assert!(second.session_exists("keep-me").unwrap());
}
