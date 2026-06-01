use std::sync::{Arc, Mutex};

use nav::{
    ChatMessage, ChatModel, Event, MockModel, ModelContext, ModelError, ModelResponse,
    SessionStore, Storage, TokenUsage, ToolDef,
};

#[test]
fn creating_a_session_emits_session_created() {
    let store = SessionStore::new(Arc::new(MockModel::new()));

    let session_id = store.create_session();
    let events = store.events(&session_id).expect("the session exists");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "session.created");
    assert_eq!(events[0].session_id, session_id);
    assert_eq!(events[0].sequence, 0);
}

fn kinds(events: &[Event]) -> Vec<&str> {
    events.iter().map(|event| event.kind.as_str()).collect()
}

#[test]
fn a_turn_emits_the_chat_event_sequence_and_records_both_messages() {
    let model = Arc::new(RecordingModel::new());
    let store = SessionStore::new(model.clone());
    let session_id = store.create_session();

    store
        .send_message(&session_id, "hello")
        .expect("the session exists");

    let events = store.events(&session_id).expect("the session exists");
    assert_eq!(
        kinds(&events),
        [
            "session.created",
            "user.message",
            "run.started",
            "message.completed",
            "run.completed",
        ],
    );

    let user_event = &events[1];
    assert_eq!(user_event.role.as_deref(), Some("user"));
    assert_eq!(user_event.text.as_deref(), Some("hello"));

    let assistant_event = &events[3];
    assert_eq!(assistant_event.role.as_deref(), Some("assistant"));
    assert_eq!(assistant_event.text.as_deref(), Some("recorded reply"));

    // The model saw exactly the user's message as its only history entry.
    let history = model.last_history();
    assert_eq!(history, vec![ChatMessage::user("hello")]);

    // Sequence numbers are dense and ordered.
    let sequences: Vec<u64> = events.iter().map(|event| event.sequence).collect();
    assert_eq!(sequences, [0, 1, 2, 3, 4]);
}

#[test]
fn a_follow_up_turn_includes_prior_messages_as_context() {
    let model = Arc::new(RecordingModel::new());
    let store = SessionStore::new(model.clone());
    let session_id = store.create_session();

    store.send_message(&session_id, "my name is Ada").unwrap();
    store.send_message(&session_id, "what is my name?").unwrap();

    // The second model call must see the full prior conversation.
    let history = model.last_history();
    assert_eq!(
        history,
        vec![
            ChatMessage::user("my name is Ada"),
            ChatMessage::assistant("recorded reply"),
            ChatMessage::user("what is my name?"),
        ],
    );
}

#[test]
fn a_model_failure_emits_run_failed_with_the_error() {
    let store = SessionStore::new(Arc::new(FailingModel));
    let session_id = store.create_session();

    store.send_message(&session_id, "hello").unwrap();

    let events = store.events(&session_id).unwrap();
    assert_eq!(
        kinds(&events),
        [
            "session.created",
            "user.message",
            "run.started",
            "run.failed"
        ],
    );
    let failure = events.last().unwrap();
    assert_eq!(failure.status.as_deref(), Some("failed"));
    assert_eq!(failure.error.as_deref(), Some("model is offline"));
}

#[test]
fn provider_token_usage_is_recorded_for_the_session() {
    let path = std::env::temp_dir().join(format!("nav_session_tokens_{}.db", uuid::Uuid::now_v7()));
    let storage = Arc::new(Storage::open(&path).expect("open storage"));
    let store = SessionStore::new(Arc::new(UsageModel)).with_storage(storage);
    let session_id = store.create_session();

    store.send_message(&session_id, "count me").unwrap();

    let conn = rusqlite::Connection::open(&path).expect("reopen db");
    let usage: (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write
             FROM sessions WHERE id = ?1",
            [session_id.as_str()],
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
        .unwrap();
    assert_eq!(usage, (21, 8, 3, 5, 0));

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn model_call_stacks_capture_context_response_and_carried_state() {
    let store = SessionStore::new(Arc::new(RecordingModel::new()));
    let session_id = store.create_session();

    store.send_message(&session_id, "show the stack").unwrap();

    let stacks = store.stacks(&session_id).expect("the session exists");
    assert_eq!(stacks.len(), 1);
    let stack = &stacks[0];
    assert_eq!(stack.sequence, 0);
    assert_eq!(stack.status, "completed");
    assert_eq!(stack.run_id.len(), 36);

    let layer = |kind: &str| {
        stack
            .layers
            .iter()
            .find(|layer| layer.kind == kind)
            .unwrap_or_else(|| panic!("missing stack layer {kind}"))
    };
    assert_eq!(layer("system_prompt").status, "available");
    assert!(
        layer("session_history").summary.contains("1 message(s)"),
        "history layer should describe the request context: {:?}",
        layer("session_history")
    );
    assert_eq!(layer("provider_payload").status, "unavailable");
    assert!(
        layer("normalized_response").summary.contains("stop finish"),
        "normalized layer should include finish reason: {:?}",
        layer("normalized_response")
    );
    assert!(
        layer("carried_forward").summary.contains("2 message(s)"),
        "carried-forward layer should include user plus assistant state: {:?}",
        layer("carried_forward")
    );
}

#[test]
fn persisted_session_summaries_include_a_workspace_root() {
    let path =
        std::env::temp_dir().join(format!("nav_session_workspace_{}.db", uuid::Uuid::now_v7()));
    let workspace = std::env::temp_dir().join(format!(
        "nav_session_workspace_root_{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let expected = workspace.to_string_lossy().replace('\\', "/");
    let storage = Arc::new(Storage::open(&path).expect("open storage"));
    storage
        .create_session("legacy-without-root", "nav")
        .expect("seed legacy session");
    let store = SessionStore::new(Arc::new(MockModel::new()))
        .with_storage(Arc::clone(&storage))
        .with_workspace(workspace.clone());

    let session_id = store.create_session();

    let conn = rusqlite::Connection::open(&path).expect("reopen db");
    let persisted_root: Option<String> = conn
        .query_row(
            "SELECT workspace_root FROM sessions WHERE id = ?1",
            [session_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted_root.as_deref(), Some(expected.as_str()));

    let summaries = store.list_sessions();
    assert!(
        summaries
            .iter()
            .all(|session| session.workspace_root.as_deref() == Some(expected.as_str())),
        "new and legacy nav sessions should list under a workspace: {summaries:?}"
    );

    drop(conn);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn missing_provider_token_usage_falls_back_to_an_estimate() {
    let path = std::env::temp_dir().join(format!(
        "nav_session_estimated_tokens_{}.db",
        uuid::Uuid::now_v7()
    ));
    let storage = Arc::new(Storage::open(&path).expect("open storage"));
    let store = SessionStore::new(Arc::new(RecordingModel::new())).with_storage(storage);
    let session_id = store.create_session();

    store.send_message(&session_id, "count me locally").unwrap();

    let conn = rusqlite::Connection::open(&path).expect("reopen db");
    let (input, output): (i64, i64) = conn
        .query_row(
            "SELECT tokens_input, tokens_output FROM sessions WHERE id = ?1",
            [session_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        input > 0,
        "fallback accounting should estimate input tokens"
    );
    assert!(
        output > 0,
        "fallback accounting should estimate output tokens"
    );

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn sending_to_an_unknown_session_is_an_error() {
    let store = SessionStore::new(Arc::new(MockModel::new()));
    assert!(store.send_message("no-such-session", "hello").is_err());
    assert!(store.events("no-such-session").is_none());
}

#[test]
fn a_subscriber_receives_backlog_then_live_events() {
    let store = SessionStore::new(Arc::new(MockModel::new()));
    let session_id = store.create_session();
    store.send_message(&session_id, "first").unwrap();

    // Subscribing replays everything already emitted.
    let subscription = store.subscribe(&session_id).expect("the session exists");
    assert_eq!(
        kinds(&subscription.backlog),
        [
            "session.created",
            "user.message",
            "run.started",
            "message.completed",
            "run.completed",
        ],
    );

    // A later turn is delivered live to the subscriber.
    store.send_message(&session_id, "second").unwrap();
    let live = subscription.next_event().expect("a live event arrives");
    assert_eq!(live.kind, "user.message");
    assert_eq!(live.text.as_deref(), Some("second"));
}

#[test]
fn a_persisted_session_resumes_with_its_history_after_a_restart() {
    let path = std::env::temp_dir().join(format!("nav_session_resume_{}.db", uuid::Uuid::now_v7()));
    let model = Arc::new(RecordingModel::new());

    // First "run" of the app: hold a conversation, persisting it.
    let session_id = {
        let storage = Arc::new(Storage::open(&path).expect("open storage"));
        let store = SessionStore::new(model.clone()).with_storage(storage);
        let session_id = store.create_session();
        store.send_message(&session_id, "my name is Ada").unwrap();
        session_id
    };

    // Second "run": a fresh store over the same database reopens the session.
    let storage = Arc::new(Storage::open(&path).expect("reopen storage"));
    let store = SessionStore::new(model.clone()).with_storage(storage);

    // The most recent session is discoverable and resumes.
    assert_eq!(
        store.latest_session_id().as_deref(),
        Some(session_id.as_str())
    );
    assert!(store.resume_session(&session_id));

    // Its backlog replays the earlier turns so the UI can redraw them.
    let events = store
        .events(&session_id)
        .expect("the session is live again");
    assert_eq!(
        kinds(&events),
        ["session.created", "user.message", "message.completed"],
    );
    assert_eq!(events[1].text.as_deref(), Some("my name is Ada"));
    assert_eq!(events[2].text.as_deref(), Some("recorded reply"));

    // A new turn carries the resumed history as context to the model.
    store.send_message(&session_id, "what is my name?").unwrap();
    assert_eq!(
        model.last_history(),
        vec![
            ChatMessage::user("my name is Ada"),
            ChatMessage::assistant("recorded reply"),
            ChatMessage::user("what is my name?"),
        ],
    );

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn resume_session_fails_when_workspace_lookup_errors() {
    let path = std::env::temp_dir().join(format!(
        "nav_session_workspace_error_{}.db",
        uuid::Uuid::now_v7()
    ));
    let model = Arc::new(RecordingModel::new());

    let session_id = {
        let storage = Arc::new(Storage::open(&path).expect("open storage"));
        let store = SessionStore::new(model.clone()).with_storage(storage);
        let session_id = store.create_session();
        store.send_message(&session_id, "persist me").unwrap();
        session_id
    };

    {
        let conn = rusqlite::Connection::open(&path).expect("reopen db");
        conn.execute("ALTER TABLE sessions DROP COLUMN workspace_root", [])
            .expect("make workspace lookup fail without breaking session/history reads");
    }

    let storage = Arc::new(Storage::open(&path).expect("reopen storage"));
    let store = SessionStore::new(model).with_storage(storage);

    assert!(!store.resume_session(&session_id));
    assert!(store.events(&session_id).is_none());

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn resuming_an_unknown_session_fails_and_latest_is_none_without_storage() {
    let store = SessionStore::new(Arc::new(MockModel::new()));
    // No storage attached: nothing to discover or resume.
    assert_eq!(store.latest_session_id(), None);
    assert!(!store.resume_session("anything"));
}

/// A model that records the history it was asked to respond to.
struct RecordingModel {
    calls: Mutex<Vec<Vec<ChatMessage>>>,
}

impl RecordingModel {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn last_history(&self) -> Vec<ChatMessage> {
        self.calls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

impl ChatModel for RecordingModel {
    fn respond(
        &self,
        context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        self.calls.lock().unwrap().push(context.messages().to_vec());
        Ok(ModelResponse::text("recorded reply"))
    }
}

/// A model that always fails, to exercise the run.failed path.
struct FailingModel;

impl ChatModel for FailingModel {
    fn respond(
        &self,
        _context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        Err(ModelError::new("model is offline"))
    }
}

struct UsageModel;

impl ChatModel for UsageModel {
    fn respond(
        &self,
        _context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        let mut response = ModelResponse::text("counted reply");
        response.token_usage = Some(TokenUsage::provider_reported(21, 8, 3, 5, 0, Some(29)));
        Ok(response)
    }
}
