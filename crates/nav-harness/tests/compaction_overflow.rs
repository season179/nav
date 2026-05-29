use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nav_harness::agents::{RunLoop, RunLoopRequest, RunLoopResult};
use nav_harness::compaction::overflow::OVERFLOW_CONTINUATION_TEXT;
use nav_harness::events::HarnessEventIdSource;
use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelRef, ModelResolver, ModelSettings,
    OpenAiCompletionsCancellationToken, OpenAiCompletionsClient, ProviderConfig,
    ResolvedModelConfig,
};
use nav_harness::sessions::{ModelTurn, ModelTurnRole, SessionStore};
use nav_harness::tools::{ToolContext, ToolPreset, ToolRegistry};
use nav_types::{ApprovalId, EventId, MessageId, RunId, SessionId, ToolCallId};

#[test]
fn overflow_continuation_appends_single_synthetic_user_turn() {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id(1);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    store
        .append_turn(&run_id, message_id(0), ModelTurn::user_text("do the thing"))
        .unwrap();
    store
        .append_turn(
            &run_id,
            message_id(1),
            ModelTurn::assistant_text("working on it"),
        )
        .unwrap();

    store
        .append_overflow_continuation(&session_id, &run_id)
        .unwrap();

    let turns = store.try_turns(&session_id).unwrap();
    let last = turns
        .last()
        .expect("a turn should exist after continuation");
    assert_eq!(last.role, ModelTurnRole::User);
    assert_eq!(last.text_content(), OVERFLOW_CONTINUATION_TEXT);
    assert_eq!(continuation_count(&turns), 1);
}

#[test]
fn overflow_continuation_is_not_duplicated_on_repeated_recovery() {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id(1);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    store
        .append_turn(&run_id, message_id(0), ModelTurn::user_text("do the thing"))
        .unwrap();

    store
        .append_overflow_continuation(&session_id, &run_id)
        .unwrap();
    store
        .append_overflow_continuation(&session_id, &run_id)
        .unwrap();

    let turns = store.try_turns(&session_id).unwrap();
    assert_eq!(continuation_count(&turns), 1);
    assert_eq!(
        turns.last().map(ModelTurn::text_content),
        Some(OVERFLOW_CONTINUATION_TEXT.to_string())
    );
}

#[test]
fn overflow_error_triggers_compaction_and_completes_on_retry() {
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_summary("## Active Task\nFinish the overflow handler."),
        canned_stream_completion("Resuming after compaction."),
    ]);
    let store = oversize_session();
    let session_id = session_id();
    let run_id = run_id(1);
    let trigger_id = message_id(99);

    let model = resolved_model(server.base_url());
    let store = Arc::new(Mutex::new(store));
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();

    let result = run_overflow_loop(&model, &store, &session_id, &run_id, &trigger_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "overflow recovery should complete on retry, got {result:?}"
    );
    assert_eq!(server.handled(), 3);

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    assert_eq!(continuation_count(&final_turns), 1);
    assert_eq!(
        last_user_turn(&final_turns).map(ModelTurn::text_content),
        Some(OVERFLOW_CONTINUATION_TEXT.to_string()),
        "the continuation prompt should be the final replayed user message"
    );
    assert!(
        final_turns
            .iter()
            .any(|turn| turn.text_content().contains("Finish the overflow handler")),
        "the compaction summary should remain in the replay window"
    );
}

#[test]
fn overflow_recovery_is_bounded_and_fails_without_looping() {
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_summary("## Active Task\nStill too large."),
        canned_context_limit(),
    ]);
    let store = oversize_session();
    let session_id = session_id();
    let run_id = run_id(1);
    let trigger_id = message_id(99);

    let model = resolved_model(server.base_url());
    let store = Arc::new(Mutex::new(store));
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();

    let result = run_overflow_loop(&model, &store, &session_id, &run_id, &trigger_id, &turns);

    assert!(
        matches!(
            result,
            RunLoopResult::Failed(nav_harness::models::OpenAiCompletionsError::ContextLimit(_))
        ),
        "a second consecutive overflow should fail the run, got {result:?}"
    );
    assert_eq!(
        server.handled(),
        3,
        "recovery should be attempted exactly once before failing"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    assert_eq!(continuation_count(&final_turns), 1);
}

fn run_overflow_loop(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    message_id: &MessageId,
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
            message_id,
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

fn oversize_session() -> SessionStore {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id(1);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    for index in 0..12 {
        let turn = if index % 2 == 0 {
            ModelTurn::user_text(format!("user turn {index} with a long oversize body"))
        } else {
            ModelTurn::assistant_text(format!("assistant turn {index} with a long oversize body"))
        };
        store.append_turn(&run_id, message_id(index), turn).unwrap();
    }
    store
        .append_turn(
            &run_id,
            message_id(99),
            ModelTurn::user_text("please finish the overflow handler"),
        )
        .unwrap();

    store
}

fn continuation_count(turns: &[ModelTurn]) -> usize {
    turns
        .iter()
        .filter(|turn| turn.text_content() == OVERFLOW_CONTINUATION_TEXT)
        .count()
}

fn last_user_turn(turns: &[ModelTurn]) -> Option<&ModelTurn> {
    turns
        .iter()
        .rev()
        .find(|turn| turn.role == ModelTurnRole::User)
}

fn resolved_model(base_url: &str) -> ResolvedModelConfig {
    let mut providers = BTreeMap::new();
    providers.insert(
        "test-provider".to_string(),
        ProviderConfig {
            name: None,
            api: ApiKind::OpenAiCompletions,
            base_url: base_url.to_string(),
            api_key: ApiKeyConfig::Inline {
                inline: "test-secret".to_string(),
            },
            models: vec![ModelConfig {
                id: "overflow-model".to_string(),
                name: None,
                api: None,
                base_url: None,
                reasoning: false,
                input: Vec::new(),
                context_window: None,
                max_tokens: None,
                compat: Default::default(),
            }],
            compat: Default::default(),
        },
    );

    ModelResolver::new(ModelSettings {
        default_model: Some(ModelRef {
            provider: "test-provider".to_string(),
            model: "overflow-model".to_string(),
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

fn canned_context_limit() -> CannedResponse {
    CannedResponse {
        status: 400,
        content_type: "application/json",
        body: r#"{"error":{"message":"This model's maximum context length is 8192 tokens","code":"context_length_exceeded"}}"#
            .to_string(),
    }
}

fn canned_summary(summary: &str) -> CannedResponse {
    let escaped = summary.replace('\\', "\\\\").replace('\n', "\\n");
    CannedResponse {
        status: 200,
        content_type: "application/json",
        body: format!(
            "{{\"choices\":[{{\"message\":{{\"role\":\"assistant\",\"content\":\"{escaped}\"}}}}]}}"
        ),
    }
}

fn canned_stream_completion(text: &str) -> CannedResponse {
    let body = format!(
        "data: {{\"id\":\"chatcmpl_retry\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"chatcmpl_retry\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
    );
    CannedResponse {
        status: 200,
        content_type: "text/event-stream",
        body,
    }
}

/// A fake provider that answers a fixed queue of canned responses, one per
/// incoming connection, so a single run-loop pass can be driven through
/// rejection, summary generation, and a successful retry.
struct FakeProviderServer {
    base_url: String,
    handled: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
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

        let handle = thread::spawn(move || {
            for response in responses {
                // Bound the wait so a regression that stops retrying fails the
                // test cleanly instead of hanging this thread (and its join) on
                // a connection that never arrives.
                let Some(mut stream) = accept_before(&listener, Duration::from_secs(10)) else {
                    return;
                };
                drain_http_request(&mut stream);

                let header = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    reason_phrase(response.status),
                    response.content_type,
                    response.body.len(),
                );
                stream
                    .write_all(header.as_bytes())
                    .expect("response header should write");
                stream
                    .write_all(response.body.as_bytes())
                    .expect("response body should write");
                stream.flush().expect("response should flush");
                handled_in_thread.fetch_add(1, Ordering::SeqCst);
            }
        });

        Self {
            base_url,
            handled,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn handled(&self) -> usize {
        self.handled.load(Ordering::SeqCst)
    }
}

impl Drop for FakeProviderServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn accept_before(listener: &TcpListener, timeout: Duration) -> Option<TcpStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("accepted stream should set blocking");
                return Some(stream);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fake server accept failed: {error}"),
        }
    }
}

fn drain_http_request(stream: &mut TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("stream should clone"));
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).expect("header should read") == 0 {
            return;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).expect("body should read");
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Status",
    }
}

#[derive(Default)]
struct TestIds {
    next_event: u64,
    next_tool_call: u64,
}

impl HarnessEventIdSource for TestIds {
    fn next_event_id(&mut self) -> EventId {
        self.next_event += 1;
        EventId::try_new(format!("019f2f6f-f178-7a72-9f2a-{:012x}", self.next_event)).unwrap()
    }

    fn next_tool_call_id(&mut self) -> ToolCallId {
        self.next_tool_call += 1;
        ToolCallId::try_new(format!(
            "019f2f6f-f178-7a72-9f2b-{:012x}",
            self.next_tool_call
        ))
        .unwrap()
    }

    fn next_approval_id(&mut self) -> ApprovalId {
        ApprovalId::try_new("019f2f6f-f178-7a72-9f2c-000000000001").unwrap()
    }
}

fn session_id() -> SessionId {
    SessionId::try_new("019f2f6f-f178-7a72-9f28-0000000000a1").unwrap()
}

fn run_id(suffix: u64) -> RunId {
    RunId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}")).unwrap()
}

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f29-{suffix:012x}")).unwrap()
}
