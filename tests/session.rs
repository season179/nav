use std::sync::{Arc, Mutex};

use nav::{
    ChatMessage, ChatModel, Event, MockModel, ModelContext, ModelError, ModelResponse,
    SessionStore, Storage, ToolDef,
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
