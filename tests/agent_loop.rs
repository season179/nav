//! End-to-end agent-loop test: a scripted model that asks for one tool call,
//! then replies with text. Verifies the event sequence, that the tool actually
//! ran against the workspace, that the tool result persists, and that resume
//! yields a text-only history.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use nav::{
    ChatMessage, ChatModel, Event, ModelError, ModelResponse, SessionStore, Storage, ToolCall,
    ToolDef,
};

/// A throwaway directory, removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("nav_{tag}_{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn kinds(events: &[Event]) -> Vec<&str> {
    events.iter().map(|event| event.kind.as_str()).collect()
}

/// Asks for a single `ls` tool call on its first turn, then replies with text
/// once it sees the tool result in the history.
struct ScriptedModel {
    calls: AtomicUsize,
    histories: Mutex<Vec<Vec<ChatMessage>>>,
}

impl ScriptedModel {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            histories: Mutex::new(Vec::new()),
        }
    }
}

impl ChatModel for ScriptedModel {
    fn respond(
        &self,
        history: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        // The loop must advertise the coding tools to the model.
        assert!(tools.iter().any(|tool| tool.name == "ls"));
        self.histories.lock().unwrap().push(history.to_vec());

        let nth = self.calls.fetch_add(1, Ordering::SeqCst);
        if nth == 0 {
            Ok(ModelResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "ls".to_owned(),
                    arguments: "{}".to_owned(),
                }],
                finish_reason: nav::FinishReason::ToolCalls,
            })
        } else {
            Ok(ModelResponse::text("done"))
        }
    }
}

#[test]
fn a_tool_call_turn_emits_the_full_event_sequence_and_runs_the_tool() {
    let workspace = TempDir::new("agent_ws");
    fs::write(workspace.path.join("hello.txt"), "hi").expect("seed file");

    let store =
        SessionStore::new(Arc::new(ScriptedModel::new())).with_workspace(workspace.path.clone());
    let session_id = store.create_session();

    store.send_message(&session_id, "list the files").unwrap();

    let events = store.events(&session_id).unwrap();
    assert_eq!(
        kinds(&events),
        [
            "session.created",
            "user.message",
            "run.started",
            "assistant.tool_calls",
            "tool.started",
            "tool.completed",
            "message.completed",
            "run.completed",
        ],
    );

    let started = &events[4];
    assert_eq!(started.tool_name.as_deref(), Some("ls"));
    assert_eq!(started.tool_call_id.as_deref(), Some("call-1"));

    let completed = &events[5];
    assert_eq!(completed.tool_name.as_deref(), Some("ls"));
    assert!(
        completed
            .text
            .as_deref()
            .unwrap_or_default()
            .contains("hello.txt"),
        "tool output should list the seeded file: {:?}",
        completed.text
    );

    let final_message = &events[6];
    assert_eq!(final_message.role.as_deref(), Some("assistant"));
    assert_eq!(final_message.text.as_deref(), Some("done"));
}

#[test]
fn a_tool_run_persists_its_result_and_replays_on_resume() {
    let workspace = TempDir::new("agent_ws");
    fs::write(workspace.path.join("hello.txt"), "hi").expect("seed file");
    let db = TempDir::new("agent_db");
    let db_path = db.path.join("nav.db");

    let session_id = {
        let storage = Arc::new(Storage::open(&db_path).expect("open storage"));
        let store = SessionStore::new(Arc::new(ScriptedModel::new()))
            .with_storage(storage)
            .with_workspace(workspace.path.clone());
        let session_id = store.create_session();
        store.send_message(&session_id, "list the files").unwrap();
        session_id
    };

    // The tool result is persisted as a `tool_result` part under a `tool` turn.
    let conn = rusqlite::Connection::open(&db_path).expect("reopen db");
    let tool_results: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM turn_parts WHERE type = 'tool_result'",
            [],
            |row| row.get(0),
        )
        .expect("count tool_result parts");
    assert_eq!(tool_results, 1);

    let tool_turns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM turns WHERE role = 'tool'",
            [],
            |row| row.get(0),
        )
        .expect("count tool turns");
    assert_eq!(tool_turns, 1);
    drop(conn);

    // Resume in a fresh store: the full turn sequence replays, tool history
    // included, so the renderer redraws the tool line just as it saw it live.
    let storage = Arc::new(Storage::open(&db_path).expect("reopen storage"));
    let store = SessionStore::new(Arc::new(ScriptedModel::new()))
        .with_storage(storage)
        .with_workspace(workspace.path.clone());
    assert!(store.resume_session(&session_id));

    let events = store.events(&session_id).unwrap();
    assert_eq!(
        kinds(&events),
        [
            "session.created",
            "user.message",
            "assistant.tool_calls",
            "tool.started",
            "tool.completed",
            "message.completed",
        ],
    );
    assert_eq!(events[1].text.as_deref(), Some("list the files"));

    // The replayed tool line keeps its name, id, and output.
    let started = &events[3];
    assert_eq!(started.tool_name.as_deref(), Some("ls"));
    assert_eq!(started.tool_call_id.as_deref(), Some("call-1"));
    let completed = &events[4];
    assert_eq!(completed.tool_name.as_deref(), Some("ls"));
    assert_eq!(completed.tool_call_id.as_deref(), Some("call-1"));
    assert!(
        completed
            .text
            .as_deref()
            .unwrap_or_default()
            .contains("hello.txt"),
        "replayed tool output should list the seeded file: {:?}",
        completed.text
    );
    assert_eq!(events[5].text.as_deref(), Some("done"));
}
