use super::*;
use serde_json::json;
use tempfile::tempdir;

fn open_temp_store() -> (tempfile::TempDir, SessionStore) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nav.db");
    let store = SessionStore::open(Some(path)).expect("open store");
    (dir, store)
}

#[test]
fn default_db_path_uses_xdg_data_dir() {
    let expected = xdg_data_home().unwrap().join("nav").join("nav.db");
    assert_eq!(default_db_path().unwrap(), expected);
}

#[test]
fn relative_db_path_resolves_under_nav_data_dir() {
    assert_eq!(
        resolve_db_path(Some(PathBuf::from("custom.db"))).unwrap(),
        default_db_dir().unwrap().join("custom.db")
    );
}

#[test]
fn absolute_db_path_is_honored() {
    let path = std::env::temp_dir().join("custom-nav.db");
    assert_eq!(resolve_db_path(Some(path.clone())).unwrap(), path);
}

#[test]
fn memory_db_path_is_honored() {
    assert_eq!(
        resolve_db_path(Some(PathBuf::from(":memory:"))).unwrap(),
        PathBuf::from(":memory:")
    );
}

#[test]
fn schema_applies_on_fresh_temp_db() {
    let (_dir, store) = open_temp_store();
    let conn = store.conn.lock().unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        names.iter().any(|n| n == "session"),
        "expected session table, got {names:?}"
    );
    assert!(names.iter().any(|n| n == "event"));
    assert!(names.iter().any(|n| n == "turn"));
    assert!(names.iter().any(|n| n == "approval"));
    assert!(names.iter().any(|n| n == "schema_version"));

    let version: i64 = conn
        .query_row(
            "SELECT version FROM schema_version WHERE version = ?1",
            params![SCHEMA_VERSION],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn pragmas_are_set_on_open() {
    let (_dir, store) = open_temp_store();
    let conn = store.conn.lock().unwrap();
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode.to_lowercase(), "wal");

    let synchronous: i64 = conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    assert_eq!(synchronous, 1, "NORMAL = 1");

    let foreign_keys: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
}

#[test]
fn round_trip_create_append_load() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/some/project");
    let id = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", Some("default"))
        .unwrap();

    let events = vec![
        AgentEvent::UserMessage {
            text: "hi".into(),
            display_text: None,
            attachments: Vec::new(),
        },
        AgentEvent::AssistantMessageDone {
            text: "hello".into(),
        },
        AgentEvent::ToolCallStarted {
            call_id: "c1".into(),
            name: "bash".into(),
            arguments: json!({"command": "echo hi"}),
        },
        AgentEvent::ToolCallOutput {
            call_id: "c1".into(),
            output: "hi\n".into(),
            is_error: false,
        },
        AgentEvent::TurnComplete {
            usage: TurnUsage {
                tokens_input: 11,
                tokens_output: 22,
                tokens_input_cached: 3,
                tokens_reasoning: 4,
            },
        },
    ];
    for event in &events {
        store.append_event(&id, event).unwrap();
    }
    let loaded = store.load_session(&id).unwrap();
    assert_eq!(loaded, events);
}

#[test]
fn append_event_tool_call_approval_request_persists_to_approval_table() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/tmp/proj");
    let id = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();

    store
        .append_event(
            &id,
            &AgentEvent::ToolCallApprovalRequest {
                call_id: "c1".into(),
                approval_id: "a1".into(),
                tool: "bash".into(),
                command: Some(vec!["rm".into(), "-rf".into(), "build".into()]),
                path: None,
                cwd: "/ws".into(),
                reason: "dangerous_pattern".into(),
                available_decisions: vec![],
            },
        )
        .unwrap();

    let conn = store.conn.lock().unwrap();
    let (tool, command, reason, decided_at, decision): (
        String,
        Option<String>,
        String,
        Option<i64>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT tool, command, reason, decided_at, decision
             FROM approval
             WHERE session_id = ?1 AND approval_id = ?2",
            params![&id, "a1"],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("approval row missing");
    assert_eq!(tool, "bash");
    assert_eq!(reason, "dangerous_pattern");
    assert!(decided_at.is_none());
    assert!(decision.is_none());
    assert!(command.unwrap().contains("rm"));
}

#[test]
fn record_approval_decision_updates_row() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/tmp/proj");
    let id = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();
    store
        .append_event(
            &id,
            &AgentEvent::ToolCallApprovalRequest {
                call_id: "c1".into(),
                approval_id: "a1".into(),
                tool: "bash".into(),
                command: None,
                path: None,
                cwd: "/ws".into(),
                reason: "dangerous_pattern".into(),
                available_decisions: vec![],
            },
        )
        .unwrap();

    store
        .record_approval_decision(&id, "a1", "approved")
        .unwrap();

    let conn = store.conn.lock().unwrap();
    let (decided_at, decision): (Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT decided_at, decision FROM approval WHERE session_id = ?1 AND approval_id = ?2",
            params![&id, "a1"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(decided_at.is_some());
    assert_eq!(decision.as_deref(), Some("approved"));
}

#[test]
fn append_event_tool_call_blocked_writes_audit_row() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/tmp/proj");
    let id = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();

    store
        .append_event(
            &id,
            &AgentEvent::ToolCallBlocked {
                call_id: "c2".into(),
                tool: "bash".into(),
                reason: "command refused unconditionally".into(),
                rule: "unbypassable_dangerous".into(),
            },
        )
        .unwrap();

    let conn = store.conn.lock().unwrap();
    let (rule, decision): (String, Option<String>) = conn
        .query_row(
            "SELECT rule, decision FROM approval WHERE session_id = ?1 AND approval_id = ?2",
            params![&id, "c2"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(rule, "unbypassable_dangerous");
    assert!(decision.is_none());
}

#[test]
fn append_event_skips_assistant_message_delta_persists_done() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/tmp/proj");
    let id = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();

    store
        .append_event(
            &id,
            &AgentEvent::AssistantMessageDelta {
                text: "ignored".into(),
            },
        )
        .unwrap();
    store
        .append_event(
            &id,
            &AgentEvent::AssistantMessageDone {
                text: "kept".into(),
            },
        )
        .unwrap();

    let loaded = store.load_session(&id).unwrap();
    assert_eq!(
        loaded,
        vec![AgentEvent::AssistantMessageDone {
            text: "kept".into()
        }]
    );
}

#[test]
fn append_event_turn_complete_rolls_up_session_tokens() {
    let (_dir, store) = open_temp_store();
    let id = store
        .create_session(
            Path::new("/proj"),
            PROVIDER_OPENAI_RESPONSES,
            "gpt-test",
            None,
        )
        .unwrap();
    store
        .append_event(
            &id,
            &AgentEvent::TurnComplete {
                usage: TurnUsage {
                    tokens_input: 100,
                    tokens_output: 50,
                    tokens_input_cached: 20,
                    tokens_reasoning: 10,
                },
            },
        )
        .unwrap();
    store
        .append_event(
            &id,
            &AgentEvent::TurnComplete {
                usage: TurnUsage {
                    tokens_input: 5,
                    tokens_output: 3,
                    tokens_input_cached: 0,
                    tokens_reasoning: 1,
                },
            },
        )
        .unwrap();
    let summary = &store.list_sessions(None).unwrap()[0];
    assert_eq!(summary.tokens_input, 105);
    assert_eq!(summary.tokens_output, 53);
    assert_eq!(summary.tokens_input_cached, 20);
    assert_eq!(summary.tokens_reasoning, 11);
}

#[test]
fn complete_turn_reported_rolls_up_cost_and_counts() {
    let (_dir, store) = open_temp_store();
    let id = store
        .create_session(
            Path::new("/proj"),
            PROVIDER_OPENAI_RESPONSES,
            "gpt-test",
            None,
        )
        .unwrap();
    let usage = TurnUsage {
        tokens_input: 50,
        tokens_output: 25,
        tokens_input_cached: 0,
        tokens_reasoning: 0,
    };
    store
        .complete_turn(
            &id,
            "gpt-test",
            &usage,
            Some(ReportedCost {
                micros: 1_500,
                currency: "USD".into(),
            }),
        )
        .unwrap();
    store
        .complete_turn(
            &id,
            "gpt-test",
            &usage,
            Some(ReportedCost {
                micros: 500,
                currency: "USD".into(),
            }),
        )
        .unwrap();
    let summary = &store.list_sessions(None).unwrap()[0];
    assert_eq!(summary.cost_micros_reported, 2_000);
    assert_eq!(summary.turns_with_reported_cost, 2);
    assert_eq!(summary.turns_total, 2);

    // Each turn row records 'reported' regardless of session-level rollup.
    let conn = store.conn.lock().unwrap();
    let sources: Vec<String> = conn
        .prepare("SELECT cost_source FROM turn WHERE session_id = ?1 ORDER BY turn_index")
        .unwrap()
        .query_map(params![id], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        sources,
        vec!["reported".to_string(), "reported".to_string()]
    );
}

#[test]
fn complete_turn_unreported_rolls_up_only_turns_total() {
    let (_dir, store) = open_temp_store();
    let id = store
        .create_session(
            Path::new("/proj"),
            PROVIDER_OPENAI_RESPONSES,
            "gpt-test",
            None,
        )
        .unwrap();
    let usage = TurnUsage {
        tokens_input: 50,
        tokens_output: 25,
        tokens_input_cached: 0,
        tokens_reasoning: 0,
    };
    store.complete_turn(&id, "gpt-test", &usage, None).unwrap();
    store.complete_turn(&id, "gpt-test", &usage, None).unwrap();
    let summary = &store.list_sessions(None).unwrap()[0];
    assert_eq!(summary.cost_micros_reported, 0);
    assert_eq!(summary.turns_with_reported_cost, 0);
    assert_eq!(summary.turns_total, 2);

    // The constraint says every turn must record cost_source='unreported',
    // cost_micros=NULL when no cost is reported.
    let conn = store.conn.lock().unwrap();
    let (source, micros): (String, Option<i64>) = conn
            .query_row(
                "SELECT cost_source, cost_micros FROM turn WHERE session_id = ?1 ORDER BY turn_index LIMIT 1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(source, "unreported");
    assert!(micros.is_none());
}

#[test]
fn list_sessions_filters_by_cwd_and_orders_by_updated_at_desc() {
    let (_dir, store) = open_temp_store();
    let a = Path::new("/proj/a");
    let b = Path::new("/proj/b");
    let id_a_old = store
        .create_session(a, PROVIDER_OPENAI_RESPONSES, "gpt", None)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let id_b = store
        .create_session(b, PROVIDER_OPENAI_RESPONSES, "gpt", None)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let id_a_new = store
        .create_session(a, PROVIDER_OPENAI_RESPONSES, "gpt", None)
        .unwrap();

    let all = store.list_sessions(None).unwrap();
    assert_eq!(
        all.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        vec![id_a_new.clone(), id_b.clone(), id_a_old.clone()]
    );
    let only_a = store.list_sessions(Some(a)).unwrap();
    assert_eq!(
        only_a.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        vec![id_a_new, id_a_old]
    );
}

#[test]
fn session_cwd_returns_creation_cwd_regardless_of_caller() {
    // session_cwd is the contract that lets --resume re-resolve stored
    // attachment paths against the session's original workspace root even
    // when the resumed nav process is launched from a different directory.
    let (_dir, store) = open_temp_store();
    let origin = Path::new("/repo/origin");
    let id = store
        .create_session(origin, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();
    assert_eq!(store.session_cwd(&id).unwrap(), origin);
}

#[test]
fn session_cwd_errors_on_missing_session() {
    let (_dir, store) = open_temp_store();
    assert!(store.session_cwd("does-not-exist").is_err());
}

#[test]
fn load_session_skips_unknown_event_kinds() {
    // A forward-compatible session log: a newer nav writes an unknown
    // event kind, and an older nav must still be able to load the session
    // around it. Without the per-row fallback, a single unrecognised kind
    // would brick --resume for every session that contains it.
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/tmp/proj");
    let id = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();

    store
        .append_event(
            &id,
            &AgentEvent::UserMessage {
                text: "first".into(),
                display_text: None,
                attachments: Vec::new(),
            },
        )
        .unwrap();
    // Insert a row with a discriminant the current AgentEvent enum does
    // not know about. The COALESCE(MAX(seq)+1) pattern matches the
    // production INSERT in append_event.
    {
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO event (session_id, seq, created_at, kind, data)
             VALUES (
                 ?1,
                 COALESCE((SELECT MAX(seq) FROM event WHERE session_id = ?1), -1) + 1,
                 ?2, ?3, ?4
             )",
            params![
                &id,
                now_secs(),
                "future_event_kind",
                r#"{"kind":"future_event_kind","payload":42}"#,
            ],
        )
        .unwrap();
    }
    store
        .append_event(&id, &AgentEvent::AssistantMessageDone { text: "ok".into() })
        .unwrap();

    let loaded = store.load_session(&id).unwrap();
    assert_eq!(
        loaded,
        vec![
            AgentEvent::UserMessage {
                text: "first".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::AssistantMessageDone { text: "ok".into() },
        ],
        "unknown event row must be skipped, surrounding rows preserved"
    );
}
