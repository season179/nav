use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fs};

use nav::{MockModel, ModelInfo, SessionStore, Storage};
use serde_json::{Value, json};

/// An in-process backend bound to an ephemeral loopback port, driven over raw
/// TCP so tests exercise the real HTTP/SSE wire format with the deterministic
/// mock model.
struct TestBackend {
    address: String,
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = env::temp_dir().join(format!("nav_{tag}_{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl TestBackend {
    fn start() -> Self {
        Self::start_with(SessionStore::new(Arc::new(MockModel::new())))
    }

    fn start_with(store: SessionStore) -> Self {
        let store = Arc::new(store);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let address = listener.local_addr().expect("read local addr").to_string();

        thread::spawn(move || {
            let _ = nav::serve(listener, store);
        });

        Self { address }
    }

    fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(&self.address).expect("connect to backend");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        stream
    }

    /// Send one buffered request/response exchange (connection closed after).
    fn request(&self, request: &str) -> String {
        let mut stream = self.connect();
        stream
            .write_all(request.as_bytes())
            .expect("send HTTP request");
        stream.shutdown(Shutdown::Write).expect("finish request");

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read HTTP response");
        response
    }

    /// Parse the JSON-RPC response body from one `/rpc` exchange.
    fn rpc(&self, body: &str) -> Value {
        let response = self.request(&format!(
            "POST /rpc HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            body.len()
        ));
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or(&response);
        serde_json::from_str(body).expect("RPC response is valid JSON")
    }

    fn create_session(&self) -> String {
        let response = self.rpc(r#"{"jsonrpc":"2.0","id":"create","method":"session.create"}"#);
        response["result"]["sessionId"]
            .as_str()
            .expect("session.create returns a sessionId")
            .to_owned()
    }

    fn send_message(&self, session_id: &str, text: &str) {
        let request = json!({
            "jsonrpc": "2.0",
            "id": "send",
            "method": "session.sendMessage",
            "params": { "sessionId": session_id, "text": text },
        });
        let response = self.rpc(&request.to_string());
        assert_eq!(
            response["result"]["accepted"],
            Value::Bool(true),
            "sendMessage should be accepted: {response}"
        );
    }

    /// Open a live SSE connection for a session.
    fn open_events(&self, session_id: &str) -> TcpStream {
        let mut stream = self.connect();
        write!(
            stream,
            "GET /sessions/{session_id}/events HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .expect("send SSE request");
        stream
    }
}

/// One SSE event reduced to the fields the chat UI cares about.
struct SseEvent {
    kind: String,
    text: Option<String>,
}

/// Read SSE frames until `completions` terminal run events have arrived.
fn read_until_completions(stream: TcpStream, completions: usize) -> Vec<SseEvent> {
    let mut reader = BufReader::new(stream);
    let mut events = Vec::new();
    let mut seen = 0;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let Some(payload) = line.strip_prefix("data: ") else {
            continue;
        };
        let event: Value = serde_json::from_str(payload.trim()).expect("SSE data is JSON");
        let kind = event["type"].as_str().unwrap_or_default().to_owned();
        let text = event["text"].as_str().map(str::to_owned);
        let terminal = kind == "run.completed" || kind == "run.failed";
        events.push(SseEvent { kind, text });
        if terminal {
            seen += 1;
            if seen >= completions {
                break;
            }
        }
    }

    events
}

fn kinds(events: &[SseEvent]) -> Vec<&str> {
    events.iter().map(|event| event.kind.as_str()).collect()
}

#[test]
fn create_session_returns_a_session_id() {
    let backend = TestBackend::start();

    let response = backend.rpc(r#"{"jsonrpc":"2.0","id":"req-1","method":"session.create"}"#);

    assert_eq!(
        response["id"], "req-1",
        "response should echo the request id"
    );
    let session_id = response["result"]["sessionId"]
        .as_str()
        .expect("a sessionId is returned");
    assert!(!session_id.is_empty(), "sessionId should not be empty");
}

#[test]
fn create_session_accepts_cwd_and_lists_the_project_root() {
    let workspace = TempDir::new("rpc_ws");
    let db = TempDir::new("rpc_db");
    let db_path = db.path.join("nav.db");
    let storage = Arc::new(Storage::open(&db_path).expect("open storage"));
    let backend = TestBackend::start_with(
        SessionStore::new(Arc::new(MockModel::new())).with_storage(storage),
    );
    let request = json!({
        "jsonrpc": "2.0",
        "id": "create",
        "method": "session.create",
        "params": { "cwd": workspace.path },
    });

    let created = backend.rpc(&request.to_string());
    assert!(
        created["result"]["sessionId"].as_str().is_some(),
        "session.create returns a session id: {created}"
    );

    let listed = backend.rpc(r#"{"jsonrpc":"2.0","id":"list","method":"session.list"}"#);
    let expected = fs::canonicalize(&workspace.path)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    assert_eq!(
        listed["result"]["sessions"][0]["workspaceRoot"], expected,
        "session.list should expose the selected cwd as project root: {listed}"
    );
}

#[test]
fn listing_sessions_without_storage_returns_an_empty_array() {
    let backend = TestBackend::start();

    let response = backend.rpc(r#"{"jsonrpc":"2.0","id":"list","method":"session.list"}"#);

    let sessions = response["result"]["sessions"]
        .as_array()
        .expect("session.list returns a sessions array");
    assert!(
        sessions.is_empty(),
        "an in-memory backend has no persisted sessions: {response}"
    );
}

#[test]
fn model_info_returns_the_configured_metadata() {
    let model_info = ModelInfo {
        label: "Claude Opus 4.8".to_owned(),
        thinking: Some("high".to_owned()),
        context_window: Some(200_000),
        token_usage: None,
    };
    let backend = TestBackend::start_with(
        SessionStore::new(Arc::new(MockModel::new())).with_model_info(model_info),
    );

    let response = backend.rpc(r#"{"jsonrpc":"2.0","id":"model","method":"session.modelInfo"}"#);

    assert_eq!(
        response["result"]["label"], "Claude Opus 4.8",
        "session.modelInfo returns the configured model label: {response}"
    );
    assert_eq!(
        response["result"]["thinking"], "high",
        "session.modelInfo returns optional thinking level metadata: {response}"
    );
    assert_eq!(
        response["result"]["tokenUsage"]["used"], 0,
        "session.modelInfo returns initial context usage: {response}"
    );
    assert_eq!(
        response["result"]["tokenUsage"]["contextWindow"], 200_000,
        "session.modelInfo returns context window metadata: {response}"
    );
}

#[test]
fn model_info_returns_latest_session_token_usage() {
    let model_info = ModelInfo {
        label: "Mock model".to_owned(),
        thinking: None,
        context_window: Some(128_000),
        token_usage: None,
    };
    let backend = TestBackend::start_with(
        SessionStore::new(Arc::new(MockModel::new())).with_model_info(model_info),
    );
    let session_id = backend.create_session();
    let stream = backend.open_events(&session_id);

    backend.send_message(&session_id, "count this context");
    let events = read_until_completions(stream, 1);
    assert_eq!(
        events.last().map(|event| event.kind.as_str()),
        Some("run.completed")
    );

    let request = json!({
        "jsonrpc": "2.0",
        "id": "model",
        "method": "session.modelInfo",
        "params": { "sessionId": session_id },
    });
    let response = backend.rpc(&request.to_string());

    let used = response["result"]["tokenUsage"]["used"]
        .as_u64()
        .expect("session.modelInfo returns used tokens");
    assert!(
        used > 0,
        "used tokens should update after a run: {response}"
    );
    assert_eq!(response["result"]["tokenUsage"]["contextWindow"], 128_000);
}

#[test]
fn session_stacks_rpc_returns_captured_model_calls() {
    let backend = TestBackend::start();
    let session_id = backend.create_session();
    let stream = backend.open_events(&session_id);

    backend.send_message(&session_id, "capture the stack");
    let events = read_until_completions(stream, 1);
    assert_eq!(
        events.last().map(|event| event.kind.as_str()),
        Some("run.completed")
    );

    let request = json!({
        "jsonrpc": "2.0",
        "id": "stacks",
        "method": "session.stacks",
        "params": { "sessionId": session_id },
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let (response, stacks) = loop {
        let response = backend.rpc(&request.to_string());
        let stacks = response["result"]["stacks"]
            .as_array()
            .expect("session.stacks returns an array")
            .clone();
        if stacks.len() == 1 || Instant::now() >= deadline {
            break (response, stacks);
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        stacks.len(),
        1,
        "one model call should be captured: {response}"
    );
    assert_eq!(stacks[0]["sequence"], 0);
    assert_eq!(stacks[0]["status"], "completed");

    let layers = stacks[0]["layers"]
        .as_array()
        .expect("stack includes layers");
    assert!(
        layers.iter().any(|layer| layer["kind"] == "system_prompt"),
        "system prompt layer should be present: {response}"
    );
    assert!(
        layers
            .iter()
            .any(|layer| layer["kind"] == "normalized_response"),
        "normalized response layer should be present: {response}"
    );
}

#[test]
fn sending_to_an_unknown_session_is_rejected() {
    let backend = TestBackend::start();

    let request = json!({
        "jsonrpc": "2.0",
        "id": "send",
        "method": "session.sendMessage",
        "params": { "sessionId": "no-such-session", "text": "hello" },
    });
    let response = backend.rpc(&request.to_string());

    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown session"),
        "unknown sessions should be rejected: {response}"
    );
}

#[test]
fn a_multi_turn_chat_streams_user_and_assistant_events_with_context() {
    let backend = TestBackend::start();
    let session_id = backend.create_session();

    // Subscribe before sending so the whole conversation is observed live.
    let stream = backend.open_events(&session_id);

    // One turn at a time: send, await its run.completed, then send the next.
    backend.send_message(&session_id, "my name is Ada");
    let first_turn = read_until_completions(backend.open_events(&session_id), 1);
    assert_eq!(
        kinds(&first_turn),
        [
            "session.created",
            "user.message",
            "run.started",
            "message.completed",
            "run.completed",
        ],
    );

    backend.send_message(&session_id, "what is my name?");
    let events = read_until_completions(stream, 2);

    assert_eq!(
        kinds(&events),
        [
            "session.created",
            "user.message",
            "run.started",
            "message.completed",
            "run.completed",
            "user.message",
            "run.started",
            "message.completed",
            "run.completed",
        ],
    );

    // The second assistant reply proves prior context was forwarded: the mock
    // recalls the opening turn.
    let second_reply = events
        .iter()
        .filter(|event| event.kind == "message.completed")
        .nth(1)
        .and_then(|event| event.text.as_deref())
        .expect("a second assistant message");
    assert!(
        second_reply.contains("what is my name?"),
        "reply should echo the latest message: {second_reply}"
    );
    assert!(
        second_reply.contains("my name is Ada"),
        "reply should recall the earlier turn: {second_reply}"
    );
}
