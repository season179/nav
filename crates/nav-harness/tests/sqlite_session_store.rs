use std::io::Read;
use std::path::{Path, PathBuf};

use nav_harness::models::{
    Decoder, OpenAiChatCompletionsDecodeInput, OpenAiChatCompletionsDecoder,
};
use nav_harness::sessions::{
    CreateSession, DecodeStatus, NewProviderPayload, Part, ProviderPayloadDirection, RunStatus,
    SessionSettings, SqliteSessionStore, SqliteStoreError, StartRun, TokenDelta, Turn, TurnMeta,
    TurnRole,
};
use nav_types::{MessageId, RunId, SessionId, ToolCallId};

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

struct TempDataDir {
    path: PathBuf,
}

impl TempDataDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "nav-sqlite-session-store-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir(&path).expect("temp data dir should be created");
        Self { path }
    }

    fn db_path(&self) -> PathBuf {
        self.path.join("nav.db")
    }
}

impl Drop for TempDataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn session_id(value: &str) -> SessionId {
    SessionId::new_unchecked(value)
}

fn run_id(value: &str) -> RunId {
    RunId::new_unchecked(value)
}

fn message_id(value: &str) -> MessageId {
    MessageId::new_unchecked(value)
}

fn tool_call_id(value: &str) -> ToolCallId {
    ToolCallId::new_unchecked(value)
}

fn text_part(text: impl Into<String>) -> Part {
    Part::Text {
        text: text.into(),
        synthetic: None,
    }
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

fn start_minimal_run(store: &SqliteSessionStore, session_id: SessionId, run_id: RunId) {
    create_minimal_session(store, session_id.clone());
    store
        .start_run(StartRun {
            id: run_id,
            session_id,
            status: RunStatus::Running,
            trigger: Some("user".to_string()),
            started_at: 2_000,
        })
        .expect("run start should commit");
}

fn count_artifacts(store: &SqliteSessionStore) -> i64 {
    store
        .execute_write(|tx| tx.query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0)))
        .expect("artifact count should be readable")
}

#[test]
fn append_turn_persists_turn_and_parts_with_transaction_assigned_seq() {
    let db = TempDb::new("turn-append");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000339");
    let run_id = run_id("019e7000-0000-7000-8000-000000000340");
    start_minimal_run(&store, session_id, run_id.clone());

    let turn = Turn {
        id: message_id("019e7000-0000-7000-8000-000000000341"),
        run_id: run_id.clone(),
        seq: 99,
        role: TurnRole::User,
        meta: TurnMeta::default(),
        created_at: 3_000,
    };

    store
        .append_turn(turn.clone(), vec![text_part("hello storage")])
        .expect("turn append should commit");

    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable");

    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].0.id, turn.id);
    assert_eq!(turns[0].0.seq, 0);
    assert_eq!(turns[0].0.role, TurnRole::User);
    assert_eq!(turns[0].1.len(), 1);
    assert_eq!(turns[0].1[0].part, text_part("hello storage"));
}

#[test]
fn append_turns_assigns_consecutive_seq_values_per_run() {
    let db = TempDb::new("turn-append-batch");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000342");
    let run_id = run_id("019e7000-0000-7000-8000-000000000343");
    start_minimal_run(&store, session_id, run_id.clone());

    let turns_with_parts = vec![
        (
            Turn {
                id: message_id("019e7000-0000-7000-8000-000000000344"),
                run_id: run_id.clone(),
                seq: 42,
                role: TurnRole::User,
                meta: TurnMeta::default(),
                created_at: 3_100,
            },
            vec![text_part("first")],
        ),
        (
            Turn {
                id: message_id("019e7000-0000-7000-8000-000000000345"),
                run_id: run_id.clone(),
                seq: 42,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 3_200,
            },
            vec![text_part("second")],
        ),
    ];

    store
        .append_turns(&turns_with_parts)
        .expect("batch append should commit");

    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable");
    let seqs = turns.iter().map(|(turn, _)| turn.seq).collect::<Vec<_>>();
    let texts = turns
        .iter()
        .map(|(_, parts)| match &parts[0].part {
            Part::Text { text, .. } => text.as_str(),
            other => panic!("expected text part, got {other:?}"),
        })
        .collect::<Vec<_>>();

    assert_eq!(seqs, vec![0, 1]);
    assert_eq!(texts, vec!["first", "second"]);
}

#[test]
fn concurrent_append_turn_writers_assign_unique_monotonic_seq_values() {
    const WRITERS: usize = 6;
    const WRITES_EACH: usize = 8;

    let db = TempDb::new("turn-append-concurrent");
    let path = db.path().to_path_buf();
    let session_id = session_id("019e7000-0000-7000-8000-000000000346");
    let run_id = run_id("019e7000-0000-7000-8000-000000000347");
    let setup = SqliteSessionStore::open(&path).expect("setup open should succeed");
    start_minimal_run(&setup, session_id, run_id.clone());
    drop(setup);

    let handles: Vec<_> = (0..WRITERS)
        .map(|writer| {
            let path = path.clone();
            let run_id = run_id.clone();
            std::thread::spawn(move || {
                let store = SqliteSessionStore::open(&path).expect("writer open should succeed");
                for write in 0..WRITES_EACH {
                    let suffix = writer * WRITES_EACH + write;
                    store
                        .append_turn(
                            Turn {
                                id: message_id(&format!("019e7000-0000-7000-8000-{suffix:012}")),
                                run_id: run_id.clone(),
                                seq: 0,
                                role: TurnRole::User,
                                meta: TurnMeta::default(),
                                created_at: 4_000 + suffix as i64,
                            },
                            vec![text_part(format!("turn {suffix}"))],
                        )
                        .expect("append should survive writer contention");
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("writer thread should not panic");
    }

    let reader = SqliteSessionStore::open(&path).expect("reader open should succeed");
    let turns = reader
        .list_turns_for_run(&run_id)
        .expect("turns should be readable after concurrent appends");
    let seqs = turns.iter().map(|(turn, _)| turn.seq).collect::<Vec<_>>();
    let expected = (0..(WRITERS * WRITES_EACH) as u32).collect::<Vec<_>>();

    assert_eq!(seqs, expected);
}

#[test]
fn list_turns_for_session_uses_stable_cursor_pagination() {
    let db = TempDb::new("turn-pagination");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000348");
    let run_id = run_id("019e7000-0000-7000-8000-000000000349");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    for (index, created_at) in [3_000, 4_000, 5_000].into_iter().enumerate() {
        store
            .append_turn(
                Turn {
                    id: message_id(&format!("019e7000-0000-7000-8000-00000000035{index}")),
                    run_id: run_id.clone(),
                    seq: 0,
                    role: TurnRole::User,
                    meta: TurnMeta::default(),
                    created_at,
                },
                vec![text_part(format!("turn {index}"))],
            )
            .expect("turn append should commit");
    }

    let first_page = store
        .list_turns_for_session(&session_id, None, 2)
        .expect("first page should be readable");
    assert_eq!(first_page.items.len(), 2);
    assert!(first_page.more);
    assert_eq!(first_page.items[0].0.created_at, 5_000);
    assert_eq!(first_page.items[1].0.created_at, 4_000);

    store
        .append_turn(
            Turn {
                id: message_id("019e7000-0000-7000-8000-000000000360"),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::User,
                meta: TurnMeta::default(),
                created_at: 6_000,
            },
            vec![text_part("newer insert")],
        )
        .expect("newer concurrent insert should commit");

    let second_page = store
        .list_turns_for_session(&session_id, first_page.cursor, 2)
        .expect("second page should be readable");

    assert!(!second_page.more);
    assert_eq!(second_page.cursor, None);
    assert_eq!(second_page.items.len(), 1);
    assert_eq!(second_page.items[0].0.created_at, 3_000);
}

#[test]
fn update_part_replaces_stored_part_payload() {
    let db = TempDb::new("turn-part-update");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000361");
    let run_id = run_id("019e7000-0000-7000-8000-000000000362");
    start_minimal_run(&store, session_id, run_id.clone());
    let turn_id = message_id("019e7000-0000-7000-8000-000000000363");

    store
        .append_turn(
            Turn {
                id: turn_id,
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 7_000,
            },
            vec![text_part("draft")],
        )
        .expect("turn append should commit");
    let part_id = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable")[0]
        .1[0]
        .id
        .clone();

    store
        .update_part(
            &part_id,
            Part::Thinking {
                text: "replaced".to_string(),
                provider_hint: Some("reasoning".to_string()),
            },
        )
        .expect("part update should commit");

    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable");
    assert_eq!(
        turns[0].1[0].part,
        Part::Thinking {
            text: "replaced".to_string(),
            provider_hint: Some("reasoning".to_string()),
        }
    );
}

#[test]
fn update_part_delta_appends_to_json_string_field() {
    let db = TempDb::new("turn-part-delta");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000364");
    let run_id = run_id("019e7000-0000-7000-8000-000000000365");
    start_minimal_run(&store, session_id, run_id.clone());
    let turn_id = message_id("019e7000-0000-7000-8000-000000000366");

    store
        .append_turn(
            Turn {
                id: turn_id.clone(),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 7_100,
            },
            vec![text_part("hel")],
        )
        .expect("turn append should commit");
    let part_id = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable")[0]
        .1[0]
        .id
        .clone();

    store
        .update_part_delta(&turn_id, &part_id, "text", "lo")
        .expect("part delta should commit");

    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable");
    assert_eq!(turns[0].1[0].part, text_part("hello"));
}

#[test]
fn update_part_delta_appends_to_tool_result_content_field() {
    let db = TempDb::new("turn-part-delta-content");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000377");
    let run_id = run_id("019e7000-0000-7000-8000-000000000378");
    start_minimal_run(&store, session_id, run_id.clone());
    let turn_id = message_id("019e7000-0000-7000-8000-000000000379");
    let call_id = tool_call_id("019e7000-0000-7000-8000-000000000380");

    store
        .append_turn(
            Turn {
                id: turn_id.clone(),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 7_125,
            },
            vec![Part::ToolResult {
                call_id: call_id.clone(),
                content: "out".to_string(),
                raw_artifact_id: None,
                is_error: false,
            }],
        )
        .expect("turn append should commit");
    let part_id = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable")[0]
        .1[0]
        .id
        .clone();

    store
        .update_part_delta(&turn_id, &part_id, "content", "put")
        .expect("part delta should commit");

    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable");
    assert_eq!(
        turns[0].1[0].part,
        Part::ToolResult {
            call_id,
            content: "output".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }
    );
}

#[test]
fn update_part_delta_rejects_non_streaming_field_without_mutating_part() {
    let db = TempDb::new("turn-part-delta-type");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000374");
    let run_id = run_id("019e7000-0000-7000-8000-000000000375");
    start_minimal_run(&store, session_id, run_id.clone());
    let turn_id = message_id("019e7000-0000-7000-8000-000000000376");

    store
        .append_turn(
            Turn {
                id: turn_id.clone(),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 7_150,
            },
            vec![text_part("stable")],
        )
        .expect("turn append should commit");
    let part_id = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable")[0]
        .1[0]
        .id
        .clone();

    let err = store
        .update_part_delta(&turn_id, &part_id, "type", "_corrupt")
        .expect_err("non-streaming deltas should be rejected");
    assert!(
        err.to_string()
            .contains("cannot append delta to non-streaming JSON field")
    );

    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should remain readable");
    assert_eq!(turns[0].1[0].part, text_part("stable"));
}

#[test]
fn compact_part_marks_part_without_changing_payload() {
    let db = TempDb::new("turn-part-compact");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000367");
    let run_id = run_id("019e7000-0000-7000-8000-000000000368");
    start_minimal_run(&store, session_id, run_id.clone());

    store
        .append_turn(
            Turn {
                id: message_id("019e7000-0000-7000-8000-000000000369"),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 7_200,
            },
            vec![Part::ToolResult {
                call_id: tool_call_id("019e7000-0000-7000-8000-000000000370"),
                content: "large output".to_string(),
                raw_artifact_id: None,
                is_error: false,
            }],
        )
        .expect("turn append should commit");
    let part_id = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable")[0]
        .1[0]
        .id
        .clone();

    store
        .compact_part(&part_id)
        .expect("part compact should commit");

    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable");
    assert!(turns[0].1[0].compacted_at.is_some());
    assert_eq!(
        turns[0].1[0].part,
        Part::ToolResult {
            call_id: tool_call_id("019e7000-0000-7000-8000-000000000370"),
            content: "large output".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }
    );
}

#[test]
fn remove_part_deletes_only_the_target_part() {
    let db = TempDb::new("turn-part-remove");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000371");
    let run_id = run_id("019e7000-0000-7000-8000-000000000372");
    start_minimal_run(&store, session_id, run_id.clone());
    let turn_id = message_id("019e7000-0000-7000-8000-000000000373");

    store
        .append_turn(
            Turn {
                id: turn_id.clone(),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 7_300,
            },
            vec![
                text_part("keep"),
                Part::Thinking {
                    text: "remove".to_string(),
                    provider_hint: None,
                },
            ],
        )
        .expect("turn append should commit");
    let parts = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable")[0]
        .1
        .clone();

    store
        .remove_part(&turn_id, &parts[1].id)
        .expect("part removal should commit");

    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable");
    assert_eq!(turns[0].1.len(), 1);
    assert_eq!(turns[0].1[0].part, text_part("keep"));
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

#[test]
fn provider_payload_append_persists_pending_row_and_raw_bytes() {
    let data_dir = TempDataDir::new("provider-payload-append");
    let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000339");
    let run_id = run_id("019e7000-0000-7000-8000-000000000340");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    let raw_bytes = br#"{"id":"chatcmpl_1","choices":[]}"#.to_vec();
    let payload_id = store
        .append_provider_payload(NewProviderPayload {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            direction: ProviderPayloadDirection::Response,
            api_kind: "openai_chat_completions".to_string(),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.1".to_string()),
            sequence: 0,
            provider_payload_id: Some("chatcmpl_1".to_string()),
            mime: "application/json".to_string(),
            raw_bytes: raw_bytes.clone(),
            created_at: 3_000,
        })
        .expect("provider payload append should commit");

    let row = store
        .get_provider_payload(&payload_id)
        .expect("provider payload row should be readable");
    assert_eq!(row.id, payload_id);
    assert_eq!(row.session_id, session_id);
    assert_eq!(row.run_id, run_id);
    assert_eq!(row.direction, "response");
    assert_eq!(row.api_kind, "openai_chat_completions");
    assert_eq!(row.provider_id.as_deref(), Some("openai"));
    assert_eq!(row.model_id.as_deref(), Some("gpt-5.1"));
    assert_eq!(row.sequence, 0);
    assert_eq!(row.provider_payload_id.as_deref(), Some("chatcmpl_1"));
    assert_eq!(row.decode_status, "pending");
    assert_eq!(row.decoder_version, None);
    assert_eq!(row.decoded_at, None);

    let mut artifact = store
        .get_artifact(&row.artifact_id)
        .expect("raw provider envelope artifact should be readable");
    let mut stored_bytes = Vec::new();
    artifact
        .reader
        .read_to_end(&mut stored_bytes)
        .expect("artifact reader should stream bytes");

    assert_eq!(artifact.row.kind, "provider_envelope");
    assert_eq!(artifact.row.mime, "application/json");
    assert_eq!(artifact.row.sha256, row.sha256);
    assert_eq!(stored_bytes, raw_bytes);
}

#[test]
fn failed_provider_payload_append_does_not_commit_artifact_row() {
    let data_dir = TempDataDir::new("provider-payload-append-rollback");
    let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000345");
    let run_id = run_id("019e7000-0000-7000-8000-000000000346");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    store
        .append_provider_payload(NewProviderPayload {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            direction: ProviderPayloadDirection::Response,
            api_kind: "openai_chat_completions".to_string(),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.1".to_string()),
            sequence: 0,
            provider_payload_id: Some("chatcmpl_1".to_string()),
            mime: "application/json".to_string(),
            raw_bytes: br#"{"id":"chatcmpl_1"}"#.to_vec(),
            created_at: 3_000,
        })
        .expect("first provider payload append should commit");
    let artifact_count = count_artifacts(&store);

    let err = store
        .append_provider_payload(NewProviderPayload {
            session_id,
            run_id,
            direction: ProviderPayloadDirection::Response,
            api_kind: "openai_chat_completions".to_string(),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.1".to_string()),
            sequence: 0,
            provider_payload_id: Some("chatcmpl_duplicate".to_string()),
            mime: "application/json".to_string(),
            raw_bytes: br#"{"id":"different-raw-envelope"}"#.to_vec(),
            created_at: 3_001,
        })
        .expect_err("duplicate run/direction/sequence should reject the append");

    assert!(
        err.to_string().contains("UNIQUE constraint failed"),
        "unexpected error: {err}"
    );
    assert_eq!(count_artifacts(&store), artifact_count);
}

#[test]
fn provider_payload_decode_status_can_be_marked_with_decoder_version() {
    let data_dir = TempDataDir::new("provider-payload-decode-status");
    let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000341");
    let run_id = run_id("019e7000-0000-7000-8000-000000000342");
    start_minimal_run(&store, session_id.clone(), run_id.clone());
    let payload_id = store
        .append_provider_payload(NewProviderPayload {
            session_id,
            run_id,
            direction: ProviderPayloadDirection::Response,
            api_kind: "openai_chat_completions".to_string(),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.1".to_string()),
            sequence: 0,
            provider_payload_id: Some("chatcmpl_1".to_string()),
            mime: "application/json".to_string(),
            raw_bytes: br#"{"choices":[{"message":{"content":"hello","extra":true}}]}"#.to_vec(),
            created_at: 3_000,
        })
        .expect("provider payload append should commit");

    store
        .mark_provider_payload_decoded(
            &payload_id,
            "openai-chat-completions-decoder@2",
            DecodeStatus::DecodedWithUnknowns,
        )
        .expect("decode status update should commit");

    store
        .mark_provider_payload_decoded(
            &payload_id,
            "openai-chat-completions-decoder@3",
            DecodeStatus::Decoded,
        )
        .expect("re-decode status update should commit");

    let row = store
        .get_provider_payload(&payload_id)
        .expect("provider payload row should be readable");
    assert_eq!(row.decode_status, "decoded");
    assert_eq!(
        row.decoder_version.as_deref(),
        Some("openai-chat-completions-decoder@3")
    );
    assert_eq!(row.error_json, None);
    assert!(row.decoded_at.is_some());
}

#[test]
fn pending_provider_payloads_survive_restart_before_decode() {
    let data_dir = TempDataDir::new("provider-payload-crash-window");
    let session_id = session_id("019e7000-0000-7000-8000-000000000343");
    let run_id = run_id("019e7000-0000-7000-8000-000000000344");
    let payload_id = {
        let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
        start_minimal_run(&store, session_id.clone(), run_id.clone());
        store
            .append_provider_payload(NewProviderPayload {
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                direction: ProviderPayloadDirection::Response,
                api_kind: "openai_chat_completions".to_string(),
                provider_id: Some("openai".to_string()),
                model_id: Some("gpt-5.1".to_string()),
                sequence: 0,
                provider_payload_id: Some("chatcmpl_1".to_string()),
                mime: "application/json".to_string(),
                raw_bytes: br#"{"choices":[{"message":{"content":"hello"}}]}"#.to_vec(),
                created_at: 3_000,
            })
            .expect("provider payload append should commit")
    };

    let reopened = SqliteSessionStore::open(data_dir.db_path()).expect("reopen should succeed");
    let pending = reopened
        .list_pending_provider_payloads()
        .expect("pending provider payloads should be readable after restart");

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, payload_id);
    assert_eq!(pending[0].decode_status, "pending");
}

// ── FTS-01a: Text projection layer ──────────────────────────────────────────

#[test]
fn inserting_text_part_populates_turn_parts_text_projection() {
    let db = TempDb::new("text-proj-insert-text");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000400");
    let run_id = run_id("019e7000-0000-7000-8000-000000000401");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    store
        .append_turn(
            Turn {
                id: message_id("019e7000-0000-7000-8000-000000000402"),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::User,
                meta: TurnMeta::default(),
                created_at: 8_000,
            },
            vec![text_part("hello FTS projection")],
        )
        .expect("turn append should commit");

    let projections = store
        .get_turn_parts_text(&session_id)
        .expect("projection should be readable");

    assert_eq!(projections.len(), 1);
    assert_eq!(projections[0].part_type, "text");
    assert_eq!(projections[0].text, "hello FTS projection");
}

#[test]
fn inserting_tool_result_part_populates_turn_parts_text_with_content() {
    let db = TempDb::new("text-proj-insert-tool-result");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000410");
    let run_id = run_id("019e7000-0000-7000-8000-000000000411");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    store
        .append_turn(
            Turn {
                id: message_id("019e7000-0000-7000-8000-000000000412"),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 8_100,
            },
            vec![Part::ToolResult {
                call_id: tool_call_id("019e7000-0000-7000-8000-000000000413"),
                content: "file contents here".to_string(),
                raw_artifact_id: None,
                is_error: false,
            }],
        )
        .expect("turn append should commit");

    let projections = store
        .get_turn_parts_text(&session_id)
        .expect("projection should be readable");

    assert_eq!(projections.len(), 1);
    assert_eq!(projections[0].part_type, "tool_result");
    assert_eq!(projections[0].text, "file contents here");
}

#[test]
fn update_part_delta_on_text_keeps_projection_in_sync() {
    let db = TempDb::new("text-proj-delta");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000420");
    let run_id = run_id("019e7000-0000-7000-8000-000000000421");
    start_minimal_run(&store, session_id.clone(), run_id.clone());
    let turn_id = message_id("019e7000-0000-7000-8000-000000000422");

    store
        .append_turn(
            Turn {
                id: turn_id.clone(),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 8_200,
            },
            vec![text_part("hel")],
        )
        .expect("turn append should commit");

    let part_id = store.list_turns_for_run(&run_id).expect("turns readable")[0].1[0]
        .id
        .clone();

    store
        .update_part_delta(&turn_id, &part_id, "text", "lo world")
        .expect("delta should commit");

    let projections = store
        .get_turn_parts_text(&session_id)
        .expect("projection should be readable");

    assert_eq!(projections.len(), 1);
    assert_eq!(projections[0].text, "hello world");
}

#[test]
fn remove_part_cleans_up_turn_parts_text_projection() {
    let db = TempDb::new("text-proj-remove");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000430");
    let run_id = run_id("019e7000-0000-7000-8000-000000000431");
    start_minimal_run(&store, session_id.clone(), run_id.clone());
    let turn_id = message_id("019e7000-0000-7000-8000-000000000432");

    store
        .append_turn(
            Turn {
                id: turn_id.clone(),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 8_300,
            },
            vec![text_part("will be removed"), text_part("will stay")],
        )
        .expect("turn append should commit");

    let parts = store.list_turns_for_run(&run_id).expect("turns readable")[0]
        .1
        .clone();

    // Both parts should be in the projection
    let projections = store
        .get_turn_parts_text(&session_id)
        .expect("projection readable before remove");
    assert_eq!(projections.len(), 2);

    store
        .remove_part(&turn_id, &parts[0].id)
        .expect("part removal should commit");

    let projections = store
        .get_turn_parts_text(&session_id)
        .expect("projection readable after remove");
    assert_eq!(projections.len(), 1);
    assert_eq!(projections[0].text, "will stay");
}

#[test]
fn tool_call_image_step_parts_are_excluded_from_text_projection() {
    let db = TempDb::new("text-proj-excluded");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000440");
    let run_id = run_id("019e7000-0000-7000-8000-000000000441");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    store
        .append_turn(
            Turn {
                id: message_id("019e7000-0000-7000-8000-000000000442"),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 8_400,
            },
            vec![
                Part::ToolCall {
                    id: tool_call_id("019e7000-0000-7000-8000-000000000443"),
                    name: "bash".to_string(),
                    arguments: serde_json::json!({"cmd": "ls"}),
                    raw_arguments_artifact_id: None,
                },
                Part::StepStart { snapshot: None },
                Part::StepFinish {
                    reason: "stop".to_string(),
                    cost: 0.01,
                    tokens: nav_harness::sessions::TokenUsage::default(),
                    snapshot: None,
                },
                Part::Compaction {
                    auto: true,
                    tail_start_id: None,
                },
            ],
        )
        .expect("turn append should commit");

    let projections = store
        .get_turn_parts_text(&session_id)
        .expect("projection should be readable");

    assert_eq!(
        projections.len(),
        0,
        "excluded types should not appear in projection"
    );
}

#[test]
fn thinking_part_populates_turn_parts_text_with_text_field() {
    let db = TempDb::new("text-proj-thinking");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000450");
    let run_id = run_id("019e7000-0000-7000-8000-000000000451");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    store
        .append_turn(
            Turn {
                id: message_id("019e7000-0000-7000-8000-000000000452"),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 8_500,
            },
            vec![Part::Thinking {
                text: "let me reason about this".to_string(),
                provider_hint: Some("reasoning".to_string()),
            }],
        )
        .expect("turn append should commit");

    let projections = store
        .get_turn_parts_text(&session_id)
        .expect("projection should be readable");

    assert_eq!(projections.len(), 1);
    assert_eq!(projections[0].part_type, "thinking");
    assert_eq!(projections[0].text, "let me reason about this");
}

#[test]
fn text_projection_round_trip_insert_update_remove() {
    let db = TempDb::new("text-proj-round-trip");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000460");
    let run_id = run_id("019e7000-0000-7000-8000-000000000461");
    start_minimal_run(&store, session_id.clone(), run_id.clone());
    let turn_id = message_id("019e7000-0000-7000-8000-000000000462");

    store
        .append_turn(
            Turn {
                id: turn_id.clone(),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 8_600,
            },
            vec![text_part("draft"), text_part("other")],
        )
        .expect("turn append should commit");

    let parts = store.list_turns_for_run(&run_id).expect("turns readable")[0]
        .1
        .clone();
    assert_eq!(store.get_turn_parts_text(&session_id).unwrap().len(), 2);

    // Update via delta
    store
        .update_part_delta(&turn_id, &parts[0].id, "text", " revised")
        .expect("delta should commit");
    let projections = store.get_turn_parts_text(&session_id).unwrap();
    assert_eq!(projections.len(), 2);
    let revised = projections
        .iter()
        .find(|p| p.text.contains("revised"))
        .unwrap();
    assert_eq!(revised.text, "draft revised");

    // Remove
    store
        .remove_part(&turn_id, &parts[1].id)
        .expect("remove should commit");
    assert_eq!(store.get_turn_parts_text(&session_id).unwrap().len(), 1);
}

#[test]
fn update_part_type_change_removes_stale_text_projection() {
    let db = TempDb::new("text-proj-type-change");
    let store = SqliteSessionStore::open(db.path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000470");
    let run_id = run_id("019e7000-0000-7000-8000-000000000471");
    start_minimal_run(&store, session_id.clone(), run_id.clone());
    let turn_id = message_id("019e7000-0000-7000-8000-000000000472");

    store
        .append_turn(
            Turn {
                id: turn_id.clone(),
                run_id: run_id.clone(),
                seq: 0,
                role: TurnRole::Assistant,
                meta: TurnMeta::default(),
                created_at: 8_700,
            },
            vec![text_part("will become tool call")],
        )
        .expect("turn append should commit");

    let part_id = store.list_turns_for_run(&run_id).expect("turns readable")[0].1[0]
        .id
        .clone();

    assert_eq!(store.get_turn_parts_text(&session_id).unwrap().len(), 1);

    // Change from text (included) to tool_call (excluded)
    store
        .update_part(
            &part_id,
            Part::ToolCall {
                id: tool_call_id("019e7000-0000-7000-8000-000000000473"),
                name: "bash".to_string(),
                arguments: serde_json::json!({"cmd": "ls"}),
                raw_arguments_artifact_id: None,
            },
        )
        .expect("part type change should commit");

    let projections = store.get_turn_parts_text(&session_id).unwrap();
    assert_eq!(
        projections.len(),
        0,
        "stale text projection should be removed after type change"
    );
}

#[test]
fn decoded_provider_payload_appends_turn_parts_with_provenance_and_status() {
    let data_dir = TempDataDir::new("provider-payload-decoded-turns");
    let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000381");
    let run_id = run_id("019e7000-0000-7000-8000-000000000382");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    let raw_bytes = br#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":"hello","vendor_extra":{"nested":[true,false]}},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#.to_vec();
    let payload_id = store
        .append_provider_payload(NewProviderPayload {
            session_id,
            run_id: run_id.clone(),
            direction: ProviderPayloadDirection::Response,
            api_kind: "openai_chat_completions".to_string(),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.1".to_string()),
            sequence: 0,
            provider_payload_id: Some("chatcmpl_1".to_string()),
            mime: "application/json".to_string(),
            raw_bytes: raw_bytes.clone(),
            created_at: 3_000,
        })
        .expect("provider payload append should commit");
    let payload = store
        .get_provider_payload(&payload_id)
        .expect("payload row should be readable");
    let decoded = OpenAiChatCompletionsDecoder::new()
        .decode(&OpenAiChatCompletionsDecodeInput {
            provider_payload_id: payload_id.clone(),
            raw_artifact_id: payload.artifact_id.clone(),
            run_id: run_id.clone(),
            provider_id: payload.provider_id.clone(),
            raw_json: raw_bytes,
            created_at: payload.created_at,
        })
        .expect("provider payload should decode");

    store
        .append_decoded_provider_payload(&payload_id, "openai-chat-completions-decoder@1", &decoded)
        .expect("decoded payload append should commit");

    let updated = store
        .get_provider_payload(&payload_id)
        .expect("payload status should be readable");
    assert_eq!(updated.decode_status, "decoded_with_unknowns");
    assert_eq!(
        updated.decoder_version.as_deref(),
        Some("openai-chat-completions-decoder@1")
    );

    let turns = store
        .list_turns_for_run(&run_id)
        .expect("decoded turns should be readable");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].1.len(), 2);
    assert_eq!(
        turns[0].1[0].provider_payload_id.as_ref(),
        Some(&payload_id)
    );
    assert_eq!(
        turns[0].1[0].provider_json_pointer.as_deref(),
        Some("/choices/0/message/content")
    );
    assert_eq!(
        turns[0].1[1].provider_payload_id.as_ref(),
        Some(&payload_id)
    );
    assert_eq!(
        turns[0].1[1].provider_json_pointer.as_deref(),
        Some("/choices/0/message/vendor_extra")
    );
}

#[test]
fn failed_provider_payload_decode_leaves_envelope_pending_without_turns() {
    let data_dir = TempDataDir::new("provider-payload-decode-failed");
    let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000383");
    let run_id = run_id("019e7000-0000-7000-8000-000000000384");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    let raw_bytes = br#"{"id":"chatcmpl_1","choices":["#.to_vec();
    let payload_id = store
        .append_provider_payload(NewProviderPayload {
            session_id,
            run_id: run_id.clone(),
            direction: ProviderPayloadDirection::Response,
            api_kind: "openai_chat_completions".to_string(),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.1".to_string()),
            sequence: 0,
            provider_payload_id: Some("chatcmpl_1".to_string()),
            mime: "application/json".to_string(),
            raw_bytes: raw_bytes.clone(),
            created_at: 3_000,
        })
        .expect("provider payload append should commit");
    let payload = store
        .get_provider_payload(&payload_id)
        .expect("payload row should be readable");

    let decode_error = OpenAiChatCompletionsDecoder::new()
        .decode(&OpenAiChatCompletionsDecodeInput {
            provider_payload_id: payload_id.clone(),
            raw_artifact_id: payload.artifact_id,
            run_id: run_id.clone(),
            provider_id: payload.provider_id,
            raw_json: raw_bytes,
            created_at: payload.created_at,
        })
        .expect_err("malformed provider JSON should fail before save");
    assert!(
        decode_error.to_string().contains("malformed JSON"),
        "unexpected error: {decode_error}"
    );

    let row = store
        .get_provider_payload(&payload_id)
        .expect("payload should remain readable");
    assert_eq!(row.decode_status, "pending");
    assert_eq!(row.decoder_version, None);
    assert!(
        store
            .list_turns_for_run(&run_id)
            .expect("turns should be readable")
            .is_empty()
    );
}

#[test]
fn decoded_provider_payload_save_failure_does_not_commit_turns() {
    let data_dir = TempDataDir::new("provider-payload-save-rollback");
    let store = SqliteSessionStore::open(data_dir.db_path()).expect("open should succeed");
    let session_id = session_id("019e7000-0000-7000-8000-000000000385");
    let run_id = run_id("019e7000-0000-7000-8000-000000000386");
    start_minimal_run(&store, session_id.clone(), run_id.clone());

    let raw_bytes = br#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}]}"#.to_vec();
    let payload_id = store
        .append_provider_payload(NewProviderPayload {
            session_id,
            run_id: run_id.clone(),
            direction: ProviderPayloadDirection::Response,
            api_kind: "openai_chat_completions".to_string(),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.1".to_string()),
            sequence: 0,
            provider_payload_id: Some("chatcmpl_1".to_string()),
            mime: "application/json".to_string(),
            raw_bytes: raw_bytes.clone(),
            created_at: 3_000,
        })
        .expect("provider payload append should commit");
    let payload = store
        .get_provider_payload(&payload_id)
        .expect("payload row should be readable");
    let decoded = OpenAiChatCompletionsDecoder::new()
        .decode(&OpenAiChatCompletionsDecodeInput {
            provider_payload_id: payload_id,
            raw_artifact_id: payload.artifact_id,
            run_id: run_id.clone(),
            provider_id: payload.provider_id,
            raw_json: raw_bytes,
            created_at: payload.created_at,
        })
        .expect("provider payload should decode");
    let missing_payload_id =
        nav_types::ProviderPayloadId::try_new("pay_0000018bcfe56800_0000000000000099")
            .expect("missing payload id should be shaped correctly");

    let err = store
        .append_decoded_provider_payload(
            &missing_payload_id,
            "openai-chat-completions-decoder@1",
            &decoded,
        )
        .expect_err("missing payload row should reject the save");

    assert!(
        err.to_string().contains("different provider payload")
            || err.to_string().contains("not found")
            || err.to_string().contains("query returned no rows"),
        "unexpected error: {err}"
    );
    assert!(
        store
            .list_turns_for_run(&run_id)
            .expect("turns should be readable")
            .is_empty()
    );
}
