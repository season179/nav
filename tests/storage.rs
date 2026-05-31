//! Durable session storage: a fresh database is created from the canonical
//! schema, and a full exchange is persisted into the shared table shapes.

use nav::{ChatMessage, Role, Storage, TokenUsage, ToolCall};
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
        history.as_turns(),
        vec![
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("second question"),
        ]
    );
    // Sanity on roles to guard the user/assistant mapping.
    assert_eq!(history.as_turns()[1].role, Role::Assistant);
}

#[test]
fn load_history_reconstructs_tool_calls_and_results_for_resume() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());
    let storage = Storage::open(&path).expect("open database");

    let session_id = "tool-session";
    storage.create_session(session_id, "nav").unwrap();
    storage.start_run("run", session_id).unwrap();

    // User asks; assistant requests two tools and carries provider thinking;
    // each tool result is recorded — one succeeds, one fails; assistant replies.
    storage
        .record_user_text(session_id, "run", 0, "do the thing")
        .unwrap();
    let calls = vec![
        ToolCall {
            id: "call-a".to_owned(),
            name: "ls".to_owned(),
            arguments: "{}".to_owned(),
        },
        ToolCall {
            id: "call-b".to_owned(),
            name: "read".to_owned(),
            arguments: r#"{"path":"x"}"#.to_owned(),
        },
    ];
    storage
        .record_assistant_tool_calls_with_reasoning(
            session_id,
            "run",
            1,
            (Some("on it"), Some("I should inspect the workspace.")),
            &calls,
            Some("m"),
        )
        .unwrap();
    storage
        .record_tool_result(session_id, "run", 2, "call-a", "a.txt\nb.txt", false)
        .unwrap();
    storage
        .record_tool_result(session_id, "run", 3, "call-b", "no such file", true)
        .unwrap();
    storage
        .record_assistant_text(session_id, "run", 4, "all done", Some("m"))
        .unwrap();
    storage.complete_run("run").unwrap();

    let history = storage.load_history(session_id).unwrap();
    assert_eq!(
        history.as_turns(),
        vec![
            ChatMessage::user("do the thing"),
            ChatMessage::assistant_tool_calls_with_reasoning(
                "on it",
                calls,
                "I should inspect the workspace.",
            ),
            ChatMessage::tool_result("call-a", "a.txt\nb.txt", false),
            ChatMessage::tool_result("call-b", "no such file", true),
            ChatMessage::assistant("all done"),
        ]
    );
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
fn token_usage_accumulates_on_the_session() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());
    let storage = Storage::open(&path).expect("open database");

    storage.create_session("s", "nav").unwrap();
    storage
        .record_token_usage(
            "s",
            &TokenUsage::provider_reported(10, 4, 2, 3, 1, Some(14)),
        )
        .unwrap();
    storage
        .record_token_usage("s", &TokenUsage::provider_reported(5, 6, 0, 1, 0, Some(11)))
        .unwrap();

    let conn = Connection::open(&path).expect("reopen for assertions");
    let usage: (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write
             FROM sessions WHERE id = 's'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap();
    assert_eq!(usage, (15, 10, 2, 4, 1));
}

#[test]
fn list_sessions_orders_by_recency_and_titles_from_first_user_message() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());
    let storage = Storage::open(&path).expect("open database");

    storage.create_session("old", "nav").unwrap();
    storage.start_run("r-old", "old").unwrap();
    storage
        .record_user_text("old", "r-old", 0, "first question about cats")
        .unwrap();
    storage
        .record_assistant_text("old", "r-old", 1, "an answer", Some("m"))
        .unwrap();
    storage.complete_run("r-old").unwrap();

    storage.create_session("new", "nav").unwrap();
    storage.start_run("r-new", "new").unwrap();
    storage
        .record_user_text("new", "r-new", 0, "newest question")
        .unwrap();
    storage.complete_run("r-new").unwrap();

    // A session with no turns yet — lists with no title.
    storage.create_session("empty", "nav").unwrap();

    // Pin distinct recency so ordering is deterministic regardless of clock
    // granularity.
    {
        let conn = Connection::open(&path).expect("reopen to set updated_at");
        conn.execute("UPDATE sessions SET updated_at = 100 WHERE id = 'old'", [])
            .unwrap();
        conn.execute("UPDATE sessions SET updated_at = 300 WHERE id = 'new'", [])
            .unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = 200 WHERE id = 'empty'",
            [],
        )
        .unwrap();
    }

    let sessions = storage.list_sessions("nav").unwrap();
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["new", "empty", "old"], "most recently updated first");

    let title = |id: &str| {
        sessions
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.title.clone())
    };
    assert_eq!(title("old").as_deref(), Some("first question about cats"));
    assert_eq!(title("new").as_deref(), Some("newest question"));
    assert_eq!(title("empty"), None, "an untouched session has no title");
}

#[test]
fn opening_a_foreign_non_nav_database_is_refused() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());

    // A populated database that is not a nav/pi database.
    {
        let conn = Connection::open(&path).expect("create foreign database");
        conn.execute_batch("CREATE TABLE widgets (id INTEGER PRIMARY KEY);")
            .unwrap();
    }

    assert!(
        Storage::open(&path).is_err(),
        "nav must not scribble its schema into an unrelated database"
    );
}

#[test]
fn load_history_collapses_a_multi_part_turn_into_one_message() {
    let path = temp_db();
    let _cleanup = TempDb(path.clone());
    let storage = Storage::open(&path).expect("open database");

    storage.create_session("s", "nav").unwrap();
    storage.start_run("r", "s").unwrap();
    storage.record_user_text("s", "r", 0, "hello").unwrap();

    // The shared schema allows several parts per turn; inject a second one.
    {
        let conn = Connection::open(&path).expect("reopen to inject a part");
        let turn_id: String = conn
            .query_row(
                "SELECT id FROM turns WHERE run_id = 'r' AND seq = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO turn_parts (id, turn_id, session_id, type, data_json, created_at)
             VALUES ('part-2', ?1, 's', 'text', ?2, 99999999999999)",
            rusqlite::params![turn_id, r#"{"type":"text","text":"world"}"#],
        )
        .unwrap();
    }

    let history = storage.load_history("s").unwrap();
    assert_eq!(
        history.as_turns().len(),
        1,
        "both parts belong to a single turn"
    );
    assert!(history.as_turns()[0].content.contains("hello"));
    assert!(history.as_turns()[0].content.contains("world"));
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
