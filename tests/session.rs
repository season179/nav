use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nav::{
    ChatMessage, ChatModel, Event, MockModel, ModelContext, ModelError, ModelInfo, ModelResponse,
    SessionStore, StackStore, Storage, TokenUsage, ToolDef,
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

fn model_info(label: &str) -> ModelInfo {
    ModelInfo {
        label: label.to_owned(),
        provider: None,
        model: None,
        thinking: None,
        thinking_levels: Vec::new(),
        context_window: None,
        token_usage: None,
    }
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
fn replacing_the_model_updates_later_turns_and_metadata() {
    let first_model = Arc::new(StaticModel("first reply"));
    let second_model = Arc::new(StaticModel("second reply"));
    let store = SessionStore::new(first_model)
        .with_model_id(Some("first-model".to_owned()))
        .with_model_info(model_info("First model"));
    let session_id = store.create_session();

    store.send_message(&session_id, "hello").unwrap();
    store.replace_model(
        second_model,
        Some("second-model".to_owned()),
        model_info("Second model"),
    );
    store.send_message(&session_id, "again").unwrap();

    let events = store.events(&session_id).unwrap();
    assert!(
        events.iter().any(|event| event.kind == "message.completed"
            && event.text.as_deref() == Some("first reply"))
    );
    assert!(
        events.iter().any(|event| event.kind == "message.completed"
            && event.text.as_deref() == Some("second reply"))
    );
    assert_eq!(store.model_info(None).label, "Second model");
}

#[test]
fn replacing_the_model_updates_the_persisted_assistant_model_id() {
    let path = std::env::temp_dir().join(format!("nav_session_model_{}.db", uuid::Uuid::now_v7()));
    let storage = Arc::new(Storage::open(&path).expect("open storage"));
    let store = SessionStore::new(Arc::new(StaticModel("first reply")))
        .with_model_id(Some("first-model".to_owned()))
        .with_model_info(model_info("First model"))
        .with_storage(storage);
    let session_id = store.create_session();

    store.send_message(&session_id, "hello").unwrap();
    store.replace_model(
        Arc::new(StaticModel("second reply")),
        Some("second-model".to_owned()),
        model_info("Second model"),
    );
    store.send_message(&session_id, "again").unwrap();

    let conn = rusqlite::Connection::open(&path).expect("reopen db");
    let models: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT model_id FROM turns WHERE role = 'assistant' ORDER BY id")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };

    assert_eq!(models, ["first-model", "second-model"]);
    let _ = std::fs::remove_file(path);
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
fn model_call_stacks_capture_the_request_sent_and_the_response_received() {
    let path =
        std::env::temp_dir().join(format!("nav_session_stacks_{}.jsonl", uuid::Uuid::now_v7()));
    let stack_store = Arc::new(StackStore::open(&path, 1024 * 1024).expect("open stack store"));
    let store = SessionStore::new(Arc::new(MockModel::new())).with_stack_store(stack_store);
    let session_id = store.create_session();

    store.send_message(&session_id, "show the stack").unwrap();

    assert!(
        store
            .stack_availability(&session_id)
            .expect("the session exists")
            .available
    );
    let result = store.stacks(&session_id).expect("the session exists");
    assert_eq!(result.unavailable_reason, None);
    let stacks = result.stacks;
    assert_eq!(stacks.len(), 1);
    let stack = &stacks[0];
    assert_eq!(stack.sequence, 0);
    assert_eq!(stack.status, "completed");
    assert_eq!(stack.run_id.len(), 36);

    // The request body holds exactly what was sent: the system prompt leads, the
    // user's message follows.
    let request_body = stack.request.body.as_ref().expect("a captured request");
    let messages = request_body["messages"].as_array().expect("messages array");
    assert_eq!(messages[0]["role"], "system");
    assert!(
        messages
            .iter()
            .any(|message| message["content"] == "show the stack"),
        "request should carry the user message verbatim: {messages:?}"
    );

    // The response body holds what came back, with no captured error.
    assert_eq!(stack.response.status_code, Some(200));
    assert!(
        stack.response.body.is_some(),
        "a successful call should capture a response body"
    );
    assert_eq!(stack.response.error, None);

    let _ = std::fs::remove_file(path);
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
fn git_worktree_sessions_list_under_the_main_checkout() {
    let path = std::env::temp_dir().join(format!(
        "nav_session_git_worktree_{}.db",
        uuid::Uuid::now_v7()
    ));
    let repo = fake_git_worktree();
    let main_root = workspace_string(
        &std::fs::canonicalize(&repo.main_root).expect("canonicalize main checkout"),
    );
    let worktree_root = workspace_string(&repo.worktree_root);
    let storage = Arc::new(Storage::open(&path).expect("open storage"));

    storage
        .create_session_with_workspace("new-worktree", "nav", Some(&repo.worktree_root))
        .expect("persist worktree session");

    let conn = rusqlite::Connection::open(&path).expect("reopen db");
    let persisted_root: Option<String> = conn
        .query_row(
            "SELECT workspace_root FROM sessions WHERE id = 'new-worktree'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted_root.as_deref(), Some(worktree_root.as_str()));

    storage
        .create_session_with_workspace("legacy-worktree", "nav", Some(&repo.main_root))
        .expect("persist legacy session");
    conn.execute(
        "UPDATE sessions SET workspace_root = ?1 WHERE id = 'legacy-worktree'",
        [&worktree_root],
    )
    .expect("seed pre-normalization worktree root");

    assert_eq!(
        storage
            .session_workspace_root("legacy-worktree")
            .expect("read legacy workspace")
            .as_deref(),
        Some(worktree_root.as_str()),
    );

    let store = SessionStore::new(Arc::new(MockModel::new()))
        .with_storage(Arc::clone(&storage))
        .with_workspace(repo.worktree_root.clone());
    let summaries = store.list_sessions();
    assert!(
        summaries
            .iter()
            .all(|session| session.project_root.as_deref() == Some(main_root.as_str())),
        "git worktree sessions should group under the main checkout: {summaries:?}"
    );

    drop(conn);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let _ = std::fs::remove_dir_all(repo.temp_root);
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

struct FakeGitWorktree {
    temp_root: PathBuf,
    main_root: PathBuf,
    worktree_root: PathBuf,
}

fn fake_git_worktree() -> FakeGitWorktree {
    let temp_root =
        std::env::temp_dir().join(format!("nav_fake_worktree_{}", uuid::Uuid::now_v7()));
    let main_root = temp_root.join("Personal").join("nav");
    let worktree_root = temp_root
        .join(".codex")
        .join("worktrees")
        .join("8f49")
        .join("nav");
    let worktree_git_dir = main_root.join(".git").join("worktrees").join("nav-8f49");

    std::fs::create_dir_all(&worktree_root).expect("create linked worktree");
    std::fs::create_dir_all(&worktree_git_dir).expect("create git worktree metadata");
    std::fs::write(
        worktree_root.join(".git"),
        format!("gitdir: {}\n", worktree_git_dir.display()),
    )
    .expect("write worktree git pointer");
    std::fs::write(worktree_git_dir.join("commondir"), "../..")
        .expect("write git common dir pointer");

    FakeGitWorktree {
        temp_root,
        main_root,
        worktree_root,
    }
}

fn workspace_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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

struct StaticModel(&'static str);

impl ChatModel for StaticModel {
    fn respond(
        &self,
        _context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse::text(self.0))
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
