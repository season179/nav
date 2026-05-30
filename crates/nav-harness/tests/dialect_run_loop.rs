//! End-to-end run-loop coverage for non-Chat-Completions dialects.
//!
//! Each test drives `RunLoop::run` against a fake provider server that answers
//! one canned HTTP response, proving the live loop selects the right
//! encoder/transport/decoder from the resolved `ApiKind`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nav_harness::agents::{RunLoop, RunLoopRequest, RunLoopResult};
use nav_harness::events::HarnessEventIdSource;
use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelRef, ModelResolver, ModelSettings,
    OpenAiCompletionsCancellationToken, OpenAiCompletionsClient, ProviderConfig, ResolvedModelConfig,
};
use nav_harness::sessions::{ModelTurn, ModelTurnRole, SessionStore};
use nav_harness::tools::{ToolContext, ToolPreset, ToolRegistry};
use nav_types::{ApprovalId, EventId, MessageId, RunId, SessionId, ToolCallId};

#[test]
fn anthropic_messages_run_completes_end_to_end() {
    let body = r#"{
        "id": "msg_01",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "text", "text": "Hello from Anthropic!"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 3}
    }"#;
    let server = FakeProviderServer::start(vec![CannedResponse::json(body)]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "say hello");

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "anthropic run should complete, got {result:?}"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let assistant = final_turns
        .iter()
        .rev()
        .find(|turn| turn.role == ModelTurnRole::Assistant)
        .expect("an assistant turn should be persisted");
    assert_eq!(assistant.text_content(), "Hello from Anthropic!");
}

#[test]
fn openai_responses_run_completes_end_to_end() {
    let body = r#"{
        "id": "resp_01",
        "model": "gpt-test",
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "Hello from Responses!", "annotations": []}]
        }],
        "usage": {"input_tokens": 7, "output_tokens": 4}
    }"#;
    let server = FakeProviderServer::start(vec![CannedResponse::json(body)]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "say hello");

    let model = responses_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "responses run should complete, got {result:?}"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let assistant = final_turns
        .iter()
        .rev()
        .find(|turn| turn.role == ModelTurnRole::Assistant)
        .expect("an assistant turn should be persisted");
    assert_eq!(assistant.text_content(), "Hello from Responses!");
}

#[test]
fn anthropic_tool_call_round_trips_to_a_second_request() {
    // First response asks to call a tool; second response is the final answer.
    let tool_use = r#"{
        "id": "msg_tool",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "mystery_tool", "input": {"q": 1}}],
        "stop_reason": "tool_use"
    }"#;
    let final_answer = r#"{
        "id": "msg_final",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "text", "text": "All done"}],
        "stop_reason": "end_turn"
    }"#;
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(tool_use),
        CannedResponse::json(final_answer),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "use a tool");

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "tool round trip should complete, got {result:?}"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let assistant_texts: Vec<String> = final_turns
        .iter()
        .filter(|turn| turn.role == ModelTurnRole::Assistant)
        .map(|turn| turn.text_content())
        .collect();
    assert!(
        assistant_texts.iter().any(|text| text == "All done"),
        "final assistant answer should be persisted, saw {assistant_texts:?}"
    );
    assert!(
        final_turns
            .iter()
            .any(|turn| turn.role == ModelTurnRole::Tool),
        "a tool-result turn should be persisted"
    );

    // The second request must replay the tool exchange with a consistent id:
    // a real Anthropic API rejects a `tool_result` whose `tool_use_id` does not
    // match the assistant `tool_use.id` from the same conversation.
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    let second: serde_json::Value =
        serde_json::from_str(&requests[1]).expect("second request body should be JSON");
    let messages = second["messages"]
        .as_array()
        .expect("anthropic request carries a messages array");
    let tool_use_id = messages
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_use")
        .and_then(|block| block["id"].as_str())
        .expect("assistant tool_use block should be re-encoded");
    let tool_result_id = messages
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_result")
        .and_then(|block| block["tool_use_id"].as_str())
        .expect("tool_result block should be re-encoded");
    assert_eq!(
        tool_use_id, tool_result_id,
        "tool_use.id must match tool_result.tool_use_id on re-encode"
    );
}

#[test]
fn openai_responses_tool_call_round_trips_to_a_second_request() {
    // The Responses encoder resolves tool-call ids through a different path than
    // Anthropic (`function_call`/`function_call_output` items keyed by
    // `call_id`), so the id-consistency guarantee needs its own coverage.
    let function_call = r#"{
        "id": "resp_tool",
        "model": "gpt-test",
        "status": "completed",
        "output": [
            {"type": "function_call", "call_id": "call_1", "name": "mystery_tool", "arguments": "{}"}
        ]
    }"#;
    let final_answer = r#"{
        "id": "resp_final",
        "model": "gpt-test",
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "All done", "annotations": []}]
        }]
    }"#;
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(function_call),
        CannedResponse::json(final_answer),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "use a tool");

    let model = responses_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "responses tool round trip should complete, got {result:?}"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    assert!(
        final_turns
            .iter()
            .filter(|turn| turn.role == ModelTurnRole::Assistant)
            .any(|turn| turn.text_content() == "All done"),
        "final assistant answer should be persisted"
    );
    assert!(
        final_turns
            .iter()
            .any(|turn| turn.role == ModelTurnRole::Tool),
        "a tool-result turn should be persisted"
    );

    // A real Responses API rejects a `function_call_output` whose `call_id` does
    // not match the `function_call` it answers.
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    let second: serde_json::Value =
        serde_json::from_str(&requests[1]).expect("second request body should be JSON");
    let input = second["input"]
        .as_array()
        .expect("responses request carries an input array");
    let call_id = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .and_then(|item| item["call_id"].as_str())
        .expect("assistant function_call should be re-encoded");
    let output_call_id = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .and_then(|item| item["call_id"].as_str())
        .expect("function_call_output should be re-encoded");
    assert_eq!(
        call_id, output_call_id,
        "function_call.call_id must match function_call_output.call_id on re-encode"
    );
}

fn run_loop_once(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    turns: &[ModelTurn],
) -> RunLoopResult {
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());
    let registry = ToolRegistry::default();
    let context = ToolContext::default();
    let mut ids = TestIds::default();

    run_loop.run(
        model,
        RunLoopRequest {
            session_id,
            run_id,
            message_id: &message_id(0),
            turns,
            tool_registry: &registry,
            tool_preset: ToolPreset::Coding,
            tool_context: &context,
            session_store: Some(store),
            pending_confirmations: None,
            cancellation_token: OpenAiCompletionsCancellationToken::new(),
        },
        &mut ids,
        |_events| {},
    )
}

fn seed_user_turn(
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    text: &str,
) {
    let store = store.lock().unwrap();
    store.create_session(session_id.clone()).unwrap();
    store.start_run(session_id, run_id.clone()).unwrap();
    store
        .append_turn(run_id, message_id(1), ModelTurn::user_text(text))
        .unwrap();
}

fn anthropic_model(base_url: &str) -> ResolvedModelConfig {
    resolved_model(base_url, ApiKind::AnthropicMessages, "claude-test")
}

fn responses_model(base_url: &str) -> ResolvedModelConfig {
    resolved_model(base_url, ApiKind::OpenAiResponses, "gpt-test")
}

fn resolved_model(base_url: &str, api: ApiKind, model_id: &str) -> ResolvedModelConfig {
    let mut providers = BTreeMap::new();
    providers.insert(
        "test-provider".to_string(),
        ProviderConfig {
            name: None,
            api,
            base_url: base_url.to_string(),
            api_key: ApiKeyConfig::Inline {
                inline: "test-secret".to_string(),
            },
            models: vec![ModelConfig {
                id: model_id.to_string(),
                name: None,
                api: None,
                base_url: None,
                reasoning: false,
                input: Vec::new(),
                context_window: None,
                max_tokens: Some(256),
                compat: Default::default(),
            }],
            compat: Default::default(),
        },
    );

    ModelResolver::new(ModelSettings {
        default_model: Some(ModelRef {
            provider: "test-provider".to_string(),
            model: model_id.to_string(),
        }),
        providers,
    })
    .resolve_default()
    .unwrap()
}

#[derive(Clone)]
struct CannedResponse {
    status: u16,
    content_type: &'static str,
    body: String,
}

impl CannedResponse {
    fn json(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.to_string(),
        }
    }
}

/// A fake provider that answers a fixed queue of canned responses, one per
/// incoming connection.
struct FakeProviderServer {
    base_url: String,
    handle: Option<JoinHandle<()>>,
    handled: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeProviderServer {
    fn start(responses: Vec<CannedResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake server should bind");
        listener
            .set_nonblocking(true)
            .expect("fake server should set non-blocking");
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let handled = Arc::new(AtomicUsize::new(0));
        let handled_in_thread = Arc::clone(&handled);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_in_thread = Arc::clone(&requests);

        let handle = thread::spawn(move || {
            for response in responses {
                let Some(mut stream) = accept_before(&listener, Duration::from_secs(10)) else {
                    return;
                };
                let body = drain_http_request(&mut stream);
                requests_in_thread.lock().unwrap().push(body);

                let header = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    reason_phrase(response.status),
                    response.content_type,
                    response.body.len(),
                );
                stream.write_all(header.as_bytes()).expect("header writes");
                stream
                    .write_all(response.body.as_bytes())
                    .expect("body writes");
                stream.flush().expect("response flushes");
                handled_in_thread.fetch_add(1, Ordering::SeqCst);
            }
        });

        Self {
            base_url,
            handle: Some(handle),
            handled,
            requests,
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Captured request bodies, in the order the server answered them.
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeProviderServer {
    fn drop(&mut self) {
        let _ = self.handled.load(Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn accept_before(listener: &TcpListener, timeout: Duration) -> Option<TcpStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

fn drain_http_request(stream: &mut TcpStream) -> String {
    stream
        .set_nonblocking(false)
        .expect("blocking mode for read");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return String::new();
        }
        let trimmed = line.trim_end();
        if let Some(value) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
        if trimmed.is_empty() {
            break;
        }
    }
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Unknown",
    }
}

#[derive(Default)]
struct TestIds {
    counter: u64,
}

impl TestIds {
    fn next_uuid(&mut self) -> String {
        self.counter += 1;
        format!("019f2f6f-f178-7a72-9f28-{:012x}", self.counter)
    }
}

impl HarnessEventIdSource for TestIds {
    fn next_event_id(&mut self) -> EventId {
        EventId::try_new(self.next_uuid()).unwrap()
    }

    fn next_tool_call_id(&mut self) -> ToolCallId {
        ToolCallId::try_new(self.next_uuid()).unwrap()
    }

    fn next_approval_id(&mut self) -> ApprovalId {
        ApprovalId::try_new(self.next_uuid()).unwrap()
    }
}

fn session_id() -> SessionId {
    SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap()
}

fn run_id(suffix: u64) -> RunId {
    RunId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012}")).unwrap()
}

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f28-1{suffix:011}")).unwrap()
}
