//! End-to-end agent-loop test: a scripted model that asks for one tool call,
//! then replies with text. Verifies the event sequence, that the tool actually
//! ran against the workspace, that the tool result persists, and that resume
//! yields a text-only history.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nav::{
    ChatMessage, ChatModel, Event, FinishReason, ModelContext, ModelError, ModelResponse, Role,
    SessionStore, Storage, ToolCall, ToolDef,
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
        context: &ModelContext,
        tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        // The loop must advertise the coding tools to the model.
        assert!(tools.iter().any(|tool| tool.name == "ls"));
        self.histories
            .lock()
            .unwrap()
            .push(context.messages().to_vec());

        let nth = self.calls.fetch_add(1, Ordering::SeqCst);
        if nth == 0 {
            Ok(ModelResponse {
                content: None,
                reasoning_content: Some("I should inspect the workspace.".to_owned()),
                tool_calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "ls".to_owned(),
                    arguments: "{}".to_owned(),
                }],
                finish_reason: nav::FinishReason::ToolCalls,
                token_usage: None,
            })
        } else {
            Ok(ModelResponse::text("done"))
        }
    }
}

/// Asks for one long-running `bash` call on its first turn; any later turn would
/// reply with text. A cancelled run must never reach that text turn.
struct SleepThenTextModel {
    calls: AtomicUsize,
}

impl ChatModel for SleepThenTextModel {
    fn respond(
        &self,
        _context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        let nth = self.calls.fetch_add(1, Ordering::SeqCst);
        if nth == 0 {
            Ok(ModelResponse {
                content: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "bash".to_owned(),
                    arguments: r#"{"command":"sleep 30"}"#.to_owned(),
                }],
                finish_reason: FinishReason::ToolCalls,
                token_usage: None,
            })
        } else {
            Ok(ModelResponse::text("should not reach a second turn"))
        }
    }
}

/// Requests two tool calls in one turn — a long `bash` followed by a `write`.
/// A stop during the bash must skip the queued write so it never hits disk.
struct SleepThenWriteModel {
    calls: AtomicUsize,
    target: String,
}

impl ChatModel for SleepThenWriteModel {
    fn respond(
        &self,
        _context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        let nth = self.calls.fetch_add(1, Ordering::SeqCst);
        if nth == 0 {
            Ok(ModelResponse {
                content: None,
                reasoning_content: None,
                tool_calls: vec![
                    ToolCall {
                        id: "call-bash".to_owned(),
                        name: "bash".to_owned(),
                        arguments: r#"{"command":"sleep 30"}"#.to_owned(),
                    },
                    ToolCall {
                        id: "call-write".to_owned(),
                        name: "write".to_owned(),
                        arguments: serde_json::json!({
                            "path": self.target,
                            "content": "should not be written",
                        })
                        .to_string(),
                    },
                ],
                finish_reason: FinishReason::ToolCalls,
                token_usage: None,
            })
        } else {
            Ok(ModelResponse::text("should not reach a second turn"))
        }
    }
}

/// One scripted reply for [`GatedModel`].
#[derive(Clone)]
enum GatedReply {
    /// Request a single tool call.
    Tool {
        id: String,
        name: String,
        args: String,
    },
    /// Reply with plain text, ending the turn.
    Text(String),
}

/// A model the test steps one call at a time. Each `respond` records the context
/// it saw, announces that it has entered the call, then blocks until the test
/// releases that call — letting the test queue steering at a precise point and
/// assert what the next model call sees. Replies come from a fixed script.
struct GatedModel {
    script: Vec<GatedReply>,
    calls: AtomicUsize,
    histories: Mutex<Vec<Vec<ChatMessage>>>,
    gate: Mutex<Gate>,
    cv: Condvar,
}

#[derive(Default)]
struct Gate {
    /// How many calls have entered `respond` (1-based high-water mark).
    entered: usize,
    /// How many calls the test has allowed to return (1-based high-water mark).
    released: usize,
}

impl GatedModel {
    fn new(script: Vec<GatedReply>) -> Self {
        Self {
            script,
            calls: AtomicUsize::new(0),
            histories: Mutex::new(Vec::new()),
            gate: Mutex::new(Gate::default()),
            cv: Condvar::new(),
        }
    }

    /// Block until the model has entered at least its `n`th call (1-based).
    fn wait_entered(&self, n: usize) {
        let mut gate = self.gate.lock().unwrap();
        while gate.entered < n {
            gate = self.cv.wait(gate).unwrap();
        }
    }

    /// Allow the model to return from its first `n` calls (1-based).
    fn release(&self, n: usize) {
        let mut gate = self.gate.lock().unwrap();
        gate.released = gate.released.max(n);
        self.cv.notify_all();
    }
}

impl ChatModel for GatedModel {
    fn respond(
        &self,
        context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        self.histories
            .lock()
            .unwrap()
            .push(context.messages().to_vec());
        let nth = self.calls.fetch_add(1, Ordering::SeqCst);
        {
            let mut gate = self.gate.lock().unwrap();
            gate.entered = gate.entered.max(nth + 1);
            self.cv.notify_all();
            while gate.released < nth + 1 {
                gate = self.cv.wait(gate).unwrap();
            }
        }
        Ok(match self.script[nth].clone() {
            GatedReply::Tool { id, name, args } => ModelResponse {
                content: None,
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id,
                    name,
                    arguments: args,
                }],
                finish_reason: FinishReason::ToolCalls,
                token_usage: None,
            },
            GatedReply::Text(text) => ModelResponse::text(text),
        })
    }
}

/// Block until the session has emitted an event of `kind`, or panic after 5s.
fn wait_for_event(store: &SessionStore, session_id: &str, kind: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(events) = store.events(session_id)
            && events.iter().any(|event| event.kind == kind)
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {kind} event");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn stopping_a_run_cancels_it_before_the_next_model_call() {
    let workspace = TempDir::new("agent_ws");
    let model = Arc::new(SleepThenTextModel {
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(SessionStore::new(model.clone()).with_workspace(workspace.path.clone()));
    let session_id = store.create_session();

    // Run on a worker thread so the test can stop it mid-flight.
    let runner = {
        let store = Arc::clone(&store);
        let session_id = session_id.clone();
        thread::spawn(move || {
            store
                .send_message(&session_id, "run a long command")
                .unwrap()
        })
    };

    // Once bash is actually running, ask the run to stop.
    wait_for_event(&store, &session_id, "tool.started");
    assert!(
        store.stop_run(&session_id),
        "a run should be active to stop"
    );

    runner.join().expect("run thread");

    let events = store.events(&session_id).unwrap();
    let kinds = kinds(&events);
    assert_eq!(
        kinds.last().copied(),
        Some("run.cancelled"),
        "a stopped run ends with run.cancelled: {kinds:?}",
    );
    assert!(
        !kinds.contains(&"message.completed"),
        "a cancelled run emits no final assistant turn: {kinds:?}",
    );
    // The loop halted instead of calling the model a second time.
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn stopping_a_run_skips_a_queued_write_in_the_same_turn() {
    let workspace = TempDir::new("agent_ws");
    let target = workspace.path.join("must_not_exist.txt");
    let model = Arc::new(SleepThenWriteModel {
        calls: AtomicUsize::new(0),
        target: target.to_string_lossy().into_owned(),
    });
    let store = Arc::new(SessionStore::new(model.clone()).with_workspace(workspace.path.clone()));
    let session_id = store.create_session();

    let runner = {
        let store = Arc::clone(&store);
        let session_id = session_id.clone();
        thread::spawn(move || store.send_message(&session_id, "do two things").unwrap())
    };

    // Stop while the first tool (bash) is running, before the write is reached.
    wait_for_event(&store, &session_id, "tool.started");
    assert!(
        store.stop_run(&session_id),
        "a run should be active to stop"
    );

    runner.join().expect("run thread");

    // The queued write was skipped, so its file was never created.
    assert!(
        !target.exists(),
        "a cancelled run must not run a queued write tool",
    );

    // The write call still got a (failed) result so the turn stays well-formed,
    // but it was never started.
    let events = store.events(&session_id).unwrap();
    let write_started = events.iter().any(|event| {
        event.kind == "tool.started" && event.tool_call_id.as_deref() == Some("call-write")
    });
    assert!(!write_started, "the queued write must not start");
    let write_failed = events.iter().any(|event| {
        event.kind == "tool.failed" && event.tool_call_id.as_deref() == Some("call-write")
    });
    assert!(write_failed, "the skipped write still gets a result");
    assert_eq!(kinds(&events).last().copied(), Some("run.cancelled"));
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

    let thinking_parts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM turn_parts WHERE type = 'thinking'",
            [],
            |row| row.get(0),
        )
        .expect("count thinking parts");
    assert_eq!(thinking_parts, 1);

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

#[test]
fn a_message_sent_during_a_tool_batch_is_folded_into_the_same_run() {
    let workspace = TempDir::new("agent_ws");
    let model = Arc::new(GatedModel::new(vec![
        GatedReply::Tool {
            id: "call-1".to_owned(),
            name: "ls".to_owned(),
            args: "{}".to_owned(),
        },
        GatedReply::Text("done".to_owned()),
    ]));
    let store = Arc::new(SessionStore::new(model.clone()).with_workspace(workspace.path.clone()));
    let session_id = store.create_session();

    let runner = {
        let store = Arc::clone(&store);
        let session_id = session_id.clone();
        thread::spawn(move || store.send_message(&session_id, "list files").unwrap())
    };

    // While the first model call (the tool request) is parked, queue a steer
    // message. It stays pending while `ls` runs and is folded in after the batch.
    model.wait_entered(1);
    store.send_message(&session_id, "also say hi").unwrap();
    model.release(1);

    // The second model call must see the steered message in its context.
    model.wait_entered(2);
    model.release(2);
    runner.join().expect("run thread");

    let histories = model.histories.lock().unwrap();
    let second_context = &histories[1];
    assert!(
        second_context
            .iter()
            .any(|message| message.role == Role::User && message.content == "also say hi"),
        "the steered message should be folded into the same run's context: {second_context:?}",
    );

    // It stayed one run: a single start, a single terminal event at the end.
    let events = store.events(&session_id).unwrap();
    let kinds = kinds(&events);
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "run.started").count(),
        1,
        "steering must not start a second run: {kinds:?}",
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "run.completed")
            .count(),
        1,
        "the run completes exactly once: {kinds:?}",
    );
    assert_eq!(kinds.last().copied(), Some("run.completed"));
    // Both the original and the steered message were echoed as user turns.
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "user.message")
            .count(),
        2,
    );
}

#[test]
fn a_message_sent_as_the_reply_lands_continues_the_run() {
    let workspace = TempDir::new("agent_ws");
    // Two plain replies: the run would end after the first, but a steer message
    // queued before it finalizes keeps the same run going for a second reply.
    let model = Arc::new(GatedModel::new(vec![
        GatedReply::Text("first reply".to_owned()),
        GatedReply::Text("second reply".to_owned()),
    ]));
    let store = Arc::new(SessionStore::new(model.clone()).with_workspace(workspace.path.clone()));
    let session_id = store.create_session();

    let runner = {
        let store = Arc::clone(&store);
        let session_id = session_id.clone();
        thread::spawn(move || store.send_message(&session_id, "first").unwrap())
    };

    // Queue steering while the first reply is still parked in the model, so it is
    // pending when the loop decides whether to finish — driving the Continue path.
    model.wait_entered(1);
    store.send_message(&session_id, "keep going").unwrap();
    model.release(1);

    model.wait_entered(2);
    model.release(2);
    runner.join().expect("run thread");

    let histories = model.histories.lock().unwrap();
    let second_context = &histories[1];
    assert!(
        second_context
            .iter()
            .any(|message| message.role == Role::User && message.content == "keep going"),
        "the steered message should continue the same run: {second_context:?}",
    );

    let events = store.events(&session_id).unwrap();
    let kinds = kinds(&events);
    // The first reply did not finalize the run; exactly one terminal event fires.
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "run.completed")
            .count(),
        1,
        "the first reply must not complete the run while steering is queued: {kinds:?}",
    );
    assert_eq!(kinds.last().copied(), Some("run.completed"));
    // Order: first reply, then the steered user turn, then the second reply.
    let user_after_first_reply = events.iter().position(|event| {
        event.kind == "user.message" && event.text.as_deref() == Some("keep going")
    });
    let first_reply = events
        .iter()
        .position(|event| event.kind == "message.completed");
    assert!(
        matches!((first_reply, user_after_first_reply), (Some(reply), Some(steer)) if steer > reply),
        "the steered turn is recorded after the reply that triggered it: {kinds:?}",
    );
}

#[test]
fn stopping_a_run_drops_a_message_queued_during_it() {
    let workspace = TempDir::new("agent_ws");
    let model = Arc::new(SleepThenTextModel {
        calls: AtomicUsize::new(0),
    });
    let store = Arc::new(SessionStore::new(model.clone()).with_workspace(workspace.path.clone()));
    let session_id = store.create_session();

    let runner = {
        let store = Arc::clone(&store);
        let session_id = session_id.clone();
        thread::spawn(move || {
            store
                .send_message(&session_id, "run a long command")
                .unwrap()
        })
    };

    // Queue a steer message while the long tool runs, then stop the run before it
    // can be folded in.
    wait_for_event(&store, &session_id, "tool.started");
    store
        .send_message(&session_id, "never mind, also do this")
        .unwrap();
    assert!(
        store.stop_run(&session_id),
        "a run should be active to stop"
    );

    runner.join().expect("run thread");

    let events = store.events(&session_id).unwrap();
    assert_eq!(kinds(&events).last().copied(), Some("run.cancelled"));
    // The queued message was dropped with the run: it never became a user turn.
    assert!(
        !events.iter().any(|event| event.kind == "user.message"
            && event.text.as_deref() == Some("never mind, also do this")),
        "a message queued onto a cancelled run must not be folded in",
    );
}
