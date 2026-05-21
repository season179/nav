use super::*;
use crate::agent_loop::control::PendingInputMode;
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
fn stale_schema_resets_to_current_empty_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nav.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (
                 version INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             );
             INSERT INTO schema_version (version, applied_at) VALUES (2, 1);
             CREATE TABLE session (
                 id TEXT PRIMARY KEY,
                 cwd TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 model TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             INSERT INTO session (id, cwd, provider, model, created_at, updated_at)
                 VALUES ('stale-id', '/stale', 'openai-responses', 'gpt-test', 1, 1);",
        )
        .unwrap();
    }

    let store = SessionStore::open(Some(path)).unwrap();
    assert!(store.list_sessions(None).unwrap().is_empty());
    let conn = store.conn.lock().unwrap();
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
fn empty_schema_version_with_sessions_resets_to_current_empty_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nav.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (
                 version INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             );
             CREATE TABLE session (
                 id TEXT PRIMARY KEY,
                 cwd TEXT NOT NULL,
                 provider TEXT NOT NULL,
                 model TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             INSERT INTO session (id, cwd, provider, model, created_at, updated_at)
                 VALUES ('stale-id', '/stale', 'openai-responses', 'gpt-test', 1, 1);",
        )
        .unwrap();
    }

    let store = SessionStore::open(Some(path)).unwrap();
    assert!(store.list_sessions(None).unwrap().is_empty());
    let conn = store.conn.lock().unwrap();
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
            truncation: None,
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
fn round_trip_pending_queue_and_abort_events() {
    let (_dir, store) = open_temp_store();
    let id = store
        .create_session(
            Path::new("/some/project"),
            PROVIDER_OPENAI_RESPONSES,
            "gpt-test",
            None,
        )
        .unwrap();

    let events = vec![
        AgentEvent::PendingInputQueued {
            id: "pending-1".into(),
            mode: PendingInputMode::FollowUp,
            text: "next".into(),
            display_text: None,
            attachments: Vec::new(),
            skill_name: None,
        },
        AgentEvent::PendingInputEdited {
            id: "pending-1".into(),
            text: "next, but clearer".into(),
            display_text: None,
            attachments: Vec::new(),
            skill_name: None,
        },
        AgentEvent::PendingInputRemoved {
            id: "pending-2".into(),
        },
        AgentEvent::PendingInputCleared {
            ids: vec!["pending-3".into(), "pending-4".into()],
        },
        AgentEvent::PendingInputDequeued {
            id: "pending-1".into(),
            mode: PendingInputMode::FollowUp,
        },
        AgentEvent::TurnAborted {
            turn_id: "turn-1".into(),
            reason: "user interrupt".into(),
        },
    ];

    for event in &events {
        store.append_event(&id, event).unwrap();
    }

    assert_eq!(store.load_session(&id).unwrap(), events);
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
fn append_event_tool_call_approval_decision_updates_audit_row() {
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
        .append_event(
            &id,
            &AgentEvent::ToolCallApprovalDecision {
                approval_id: "a1".into(),
                decision: crate::ReviewDecision::ApprovedForSession,
            },
        )
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
    assert_eq!(decision.as_deref(), Some("approved_for_session"));
}

#[test]
fn session_store_sink_records_approval_decision_as_event() {
    let (_dir, store) = open_temp_store();
    let store = std::sync::Arc::new(store);
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

    let sink = store.sink_for(id.to_string());
    crate::guardrails::approval::DecisionRecorder::record(
        &sink,
        "a1",
        crate::ReviewDecision::Denied,
    );

    let events = store.load_session(&id).unwrap();
    assert!(matches!(
        events.last(),
        Some(AgentEvent::ToolCallApprovalDecision {
            approval_id,
            decision: crate::ReviewDecision::Denied,
        }) if approval_id == "a1"
    ));
    let conn = store.conn.lock().unwrap();
    let decision: Option<String> = conn
        .query_row(
            "SELECT decision FROM approval WHERE session_id = ?1 AND approval_id = ?2",
            params![&id, "a1"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(decision.as_deref(), Some("denied"));
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
fn list_sessions_includes_picker_metadata() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/proj/named");
    let id = store
        .create_session_named(
            cwd,
            PROVIDER_OPENAI_RESPONSES,
            "gpt",
            Some("default"),
            Some("launch name"),
        )
        .unwrap();
    store
        .append_event(
            &id,
            &AgentEvent::UserMessage {
                text: "first user prompt".into(),
                display_text: None,
                attachments: Vec::new(),
            },
        )
        .unwrap();
    store
        .complete_turn(&id, "gpt", &TurnUsage::default(), None)
        .unwrap();

    let summary = store.list_sessions(None).unwrap().remove(0);
    assert_eq!(summary.id, id);
    assert_eq!(summary.name.as_deref(), Some("launch name"));
    assert_eq!(summary.created_at, summary.last_active);
    assert_eq!(summary.updated_at, summary.last_active);
    assert_eq!(summary.turn_count, 1);
    assert_eq!(
        summary.first_user_prompt.as_deref(),
        Some("first user prompt")
    );
}

#[test]
fn set_session_name_updates_existing_session_without_requiring_uniqueness() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/proj");
    let a = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt", None)
        .unwrap();
    let b = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt", None)
        .unwrap();

    store.set_session_name(&a, "same name").unwrap();
    store.set_session_name(&b, "same name").unwrap();

    let summaries = store.list_sessions(None).unwrap();
    let names: Vec<_> = summaries.iter().map(|s| s.name.as_deref()).collect();
    assert_eq!(names, vec![Some("same name"), Some("same name")]);
}

#[test]
fn resolve_session_id_accepts_unique_prefix_and_rejects_ambiguous_prefix() {
    let (_dir, store) = open_temp_store();
    {
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO session (id, cwd, provider, model, created_at, updated_at)
             VALUES (?1, '/repo', ?2, 'gpt', 1, 1)",
            params!["01AAAA00000000000000000000", PROVIDER_OPENAI_RESPONSES],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, cwd, provider, model, created_at, updated_at)
             VALUES (?1, '/repo', ?2, 'gpt', 1, 1)",
            params!["01AAAB00000000000000000000", PROVIDER_OPENAI_RESPONSES],
        )
        .unwrap();
    }

    assert_eq!(
        store.resolve_session_id("01AAAA").unwrap(),
        "01AAAA00000000000000000000"
    );
    assert!(matches!(
        store.resolve_session_id("01AAA"),
        Err(ResolveSessionError::AmbiguousPrefix { .. })
    ));
    assert!(matches!(
        store.resolve_session_id("01ZZZ"),
        Err(ResolveSessionError::NotFound { .. })
    ));
}

#[test]
fn infer_export_format_defaults_to_markdown_and_honors_json_extension() {
    assert_eq!(infer_export_format(None, None), ExportFormat::Markdown);
    assert_eq!(
        infer_export_format(Some(Path::new("session.md")), None),
        ExportFormat::Markdown
    );
    assert_eq!(
        infer_export_format(Some(Path::new("session.json")), None),
        ExportFormat::Json
    );
    assert_eq!(
        infer_export_format(
            Some(Path::new("session.json")),
            Some(ExportFormat::Markdown)
        ),
        ExportFormat::Markdown
    );
}

#[test]
fn markdown_export_groups_turns_and_collapses_tool_calls() {
    let events = vec![
        AgentEvent::UserMessage {
            text: "inspect the repo".into(),
            display_text: None,
            attachments: Vec::new(),
        },
        AgentEvent::ToolCallStarted {
            call_id: "call-1".into(),
            name: "bash".into(),
            arguments: json!({"command": "git status --short"}),
        },
        AgentEvent::ToolCallOutput {
            call_id: "call-1".into(),
            output: " M src/main.rs\n".into(),
            is_error: false,
            truncation: None,
        },
        AgentEvent::AssistantMessageDone {
            text: "The tree is dirty.".into(),
        },
        AgentEvent::TurnComplete {
            usage: TurnUsage::default(),
        },
        AgentEvent::UserMessage {
            text: "thanks".into(),
            display_text: Some("thanks (display)".into()),
            attachments: Vec::new(),
        },
    ];

    let markdown = export_events(&events, ExportFormat::Markdown).unwrap();
    assert!(markdown.contains("# nav transcript"));
    assert!(markdown.contains("## Turn 1"));
    assert!(markdown.contains("### User"));
    assert!(markdown.contains("inspect the repo"));
    assert!(markdown.contains("<details>"));
    assert!(markdown.contains("<summary>Tool call: bash</summary>"));
    assert!(markdown.contains("git status --short"));
    assert!(markdown.contains("### Tool result"));
    assert!(markdown.contains("M src/main.rs"));
    assert!(markdown.contains("### Assistant"));
    assert!(markdown.contains("The tree is dirty."));
    assert!(markdown.contains("## Turn 2"));
    assert!(markdown.contains("thanks (display)"));
}

#[test]
fn json_export_reuses_agent_event_schema_as_array() {
    let events = vec![
        AgentEvent::UserMessage {
            text: "hello".into(),
            display_text: None,
            attachments: Vec::new(),
        },
        AgentEvent::AssistantMessageDone { text: "hi".into() },
    ];

    let json = export_events(&events, ExportFormat::Json).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value[0]["kind"], "user_message");
    assert_eq!(value[0]["text"], "hello");
    assert_eq!(value[1]["kind"], "assistant_message_done");
    assert_eq!(value[1]["text"], "hi");
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
fn fork_session_copies_events_up_to_seq_and_records_parent_link() {
    let (_dir, store) = open_temp_store();
    let parent = store
        .create_session(
            Path::new("/proj"),
            PROVIDER_OPENAI_RESPONSES,
            "gpt-test",
            None,
        )
        .unwrap();
    let baseline_events = vec![
        AgentEvent::UserMessage {
            text: "first".into(),
            display_text: None,
            attachments: Vec::new(),
        },
        AgentEvent::AssistantMessageDone {
            text: "first reply".into(),
        },
        AgentEvent::TurnComplete {
            usage: TurnUsage {
                tokens_input: 10,
                tokens_output: 5,
                tokens_input_cached: 0,
                tokens_reasoning: 0,
            },
        },
        AgentEvent::UserMessage {
            text: "second".into(),
            display_text: None,
            attachments: Vec::new(),
        },
        AgentEvent::AssistantMessageDone {
            text: "second reply".into(),
        },
        AgentEvent::TurnComplete {
            usage: TurnUsage {
                tokens_input: 7,
                tokens_output: 3,
                tokens_input_cached: 0,
                tokens_reasoning: 0,
            },
        },
    ];
    for event in &baseline_events {
        store.append_event(&parent, event).unwrap();
    }

    // Fork mid-conversation at seq 2 (after first turn_complete).
    let mid = store
        .fork_session(&parent, Some(2), Some("alt path"))
        .unwrap();
    let copied = store.load_session(&mid).unwrap();
    assert_eq!(copied, baseline_events[..3]);

    let summary = store.session_summary(&mid).unwrap().unwrap();
    assert_eq!(summary.parent_id.as_deref(), Some(parent.as_str()));
    assert_eq!(summary.name.as_deref(), Some("alt path"));
    assert_eq!(summary.tokens_input, 10);
    assert_eq!(summary.tokens_output, 5);

    // Forking with `at_seq = None` copies every event and recomputes totals
    // from each copied turn_complete payload.
    let head = store.fork_session(&parent, None, None).unwrap();
    let head_events = store.load_session(&head).unwrap();
    assert_eq!(head_events, baseline_events);
    let head_summary = store.session_summary(&head).unwrap().unwrap();
    assert_eq!(head_summary.tokens_input, 17);
    assert_eq!(head_summary.tokens_output, 8);
}

#[test]
fn labels_round_trip_and_filter_listing() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/proj");
    let alpha = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();
    let beta = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();

    store.add_label(&alpha, "release").unwrap();
    store.add_label(&alpha, "wip").unwrap();
    store.add_label(&beta, "release").unwrap();
    // Repeating an add is a no-op.
    store.add_label(&alpha, "release").unwrap();
    assert!(store.add_label(&alpha, "  ").is_err());

    let alpha_labels = store.labels_for(&alpha).unwrap();
    assert_eq!(alpha_labels, vec!["release", "wip"]);

    let release_sessions = store.list_by_label("release", None).unwrap();
    let release_ids: Vec<&str> = release_sessions.iter().map(|s| s.id.as_str()).collect();
    assert!(release_ids.contains(&alpha.as_str()));
    assert!(release_ids.contains(&beta.as_str()));

    // list_sessions surfaces labels on the summary directly.
    let listing = store.list_sessions(Some(cwd)).unwrap();
    let alpha_summary = listing.iter().find(|s| s.id == alpha).unwrap();
    assert_eq!(alpha_summary.labels, vec!["release", "wip"]);

    store.remove_label(&alpha, "wip").unwrap();
    assert_eq!(store.labels_for(&alpha).unwrap(), vec!["release"]);
}

#[test]
fn walk_tree_returns_depth_ordered_descendants() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/proj");
    let root = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();
    store
        .append_event(
            &root,
            &AgentEvent::UserMessage {
                text: "root start".into(),
                display_text: None,
                attachments: Vec::new(),
            },
        )
        .unwrap();
    let child_a = store.fork_session(&root, None, Some("child A")).unwrap();
    let child_b = store.fork_session(&root, None, Some("child B")).unwrap();
    let grand = store
        .fork_session(&child_a, None, Some("grandchild"))
        .unwrap();

    let tree = store.walk_tree(&root).unwrap();
    let depths: Vec<u32> = tree.iter().map(|node| node.depth).collect();
    let ids: Vec<&str> = tree.iter().map(|node| node.summary.id.as_str()).collect();

    assert!(
        depths.windows(2).all(|w| w[0] <= w[1]),
        "walk_tree must order by depth ascending: {depths:?}"
    );
    assert_eq!(ids[0], root);
    assert!(ids.contains(&child_a.as_str()));
    assert!(ids.contains(&child_b.as_str()));
    assert!(ids.contains(&grand.as_str()));
    // The grandchild is the only node at depth 2.
    let depth_two: Vec<&str> = tree
        .iter()
        .filter(|node| node.depth == 2)
        .map(|node| node.summary.id.as_str())
        .collect();
    assert_eq!(depth_two, vec![grand.as_str()]);

    // child_count surfaces the immediate fan-out.
    let root_summary = store.session_summary(&root).unwrap().unwrap();
    assert_eq!(root_summary.child_count, 2);
}

#[test]
fn fts_trigger_indexed_kinds_match_agent_event_kind_strings() {
    // The v3 FTS triggers (event_fts_ai / event_fts_ad) hardcode three event
    // kind strings. They must stay in lockstep with AgentEvent::kind() —
    // otherwise renaming a variant silently breaks transcript search.
    let user = AgentEvent::UserMessage {
        text: "x".into(),
        display_text: None,
        attachments: Vec::new(),
    };
    let assistant_done = AgentEvent::AssistantMessageDone { text: "y".into() };
    let assistant_delta = AgentEvent::AssistantMessageDelta { text: "z".into() };
    assert_eq!(user.kind(), "user_message");
    assert_eq!(assistant_done.kind(), "assistant_message_done");
    assert_eq!(assistant_delta.kind(), "assistant_message_delta");

    let triggers = include_str!("init.sql");
    for kind in [
        "'user_message'",
        "'assistant_message_done'",
        "'assistant_message_delta'",
    ] {
        assert!(
            triggers.contains(kind),
            "init.sql FTS triggers must list {kind} — keep them in sync with AgentEvent::kind()"
        );
    }
}

#[test]
fn search_transcript_finds_phrase_across_sessions() {
    let (_dir, store) = open_temp_store();
    let cwd = Path::new("/proj");
    let one = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();
    let two = store
        .create_session(cwd, PROVIDER_OPENAI_RESPONSES, "gpt-test", None)
        .unwrap();

    for (id, prompt, reply) in [
        (
            &one,
            "investigate purple flamingo migration",
            "looking into the purple flamingo path",
        ),
        (&two, "fix bash sandbox", "sandboxed bash on macOS only"),
    ] {
        store
            .append_event(
                id,
                &AgentEvent::UserMessage {
                    text: prompt.to_string(),
                    display_text: None,
                    attachments: Vec::new(),
                },
            )
            .unwrap();
        store
            .append_event(id, &AgentEvent::AssistantMessageDone { text: reply.into() })
            .unwrap();
    }

    let hits = store.search_transcript("flamingo", 10, None).unwrap();
    let session_ids: std::collections::HashSet<&str> =
        hits.iter().map(|h| h.session_id.as_str()).collect();
    assert!(
        session_ids.contains(one.as_str()),
        "FTS should find phrase in the first session: {session_ids:?}"
    );
    // The second session has no 'flamingo' so its events must not appear.
    assert!(!session_ids.contains(two.as_str()));

    // Label-scoped search excludes sessions without the label.
    store.add_label(&one, "investigation").unwrap();
    let labelled = store
        .search_transcript("flamingo", 10, Some("investigation"))
        .unwrap();
    assert!(labelled.iter().any(|h| h.session_id == one));

    let empty = store
        .search_transcript("flamingo", 10, Some("missing-label"))
        .unwrap();
    assert!(empty.is_empty());
}
