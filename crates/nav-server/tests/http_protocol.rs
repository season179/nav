use std::time::Duration;

use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelInput, ModelRef, ModelSettings, ProviderCompat,
    ProviderConfig,
};
use nav_protocol::EventEnvelope;
use nav_protocol::rpc::SessionSource;
use nav_server::http::{
    HttpRequest, HttpServer, HttpServerConfig, ProtocolEventSubscription, RunStatus,
};
use nav_types::{RunId, SessionId};
use serde_json::{Value, json};

mod support;

use support::{
    FakeProviderServer, HangingProviderServer, provider_sse_chunk, successful_provider_with_text,
    unused_local_base_url, wait_for_run_status,
};

const LIVE_EVENT_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn session_create_accepts_omitted_params() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());

    let create_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(100),
            "method": "session.create"
        })
        .to_string(),
    ));

    assert_eq!(create_response.status(), 200);
    let create_body: Value = serde_json::from_str(create_response.body()).unwrap();
    let session_id = create_body["result"]["sessionId"]
        .as_str()
        .expect("session.create should return a session id without params");
    assert_uuid_v7(session_id);

    let event_response =
        server.handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")));
    let events = parse_sse(event_response.body());
    assert_eq!(event_names(&events), vec!["session.created"]);
    assert_protocol_event_ids(&events, session_id);
}

#[test]
fn session_create_keeps_optional_session_metadata_outside_event_log() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());

    let create_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(101),
            "method": "session.create",
            "params": {
                "cwd": "/tmp/nav-workspace",
                "source": "tui",
                "settingsJson": {
                    "modelRef": {
                        "provider": "compatible-gateway",
                        "model": "vendor/model-large"
                    }
                }
            }
        })
        .to_string(),
    ));

    assert_eq!(create_response.status(), 200);
    let create_body: Value = serde_json::from_str(create_response.body()).unwrap();
    let session_id = SessionId::try_new(
        create_body["result"]["sessionId"]
            .as_str()
            .expect("session.create should return a session id"),
    )
    .unwrap();

    let metadata = server
        .session_metadata(&session_id)
        .expect("session metadata should be retained");
    assert_eq!(metadata.cwd(), Some("/tmp/nav-workspace"));
    assert_eq!(metadata.source(), Some(SessionSource::Tui));
    assert_eq!(
        metadata.settings_json().unwrap()["modelRef"]["provider"],
        "compatible-gateway"
    );

    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!(
                "/sessions/{}/events",
                session_id.as_str()
            )))
            .body(),
    );
    assert_eq!(event_names(&events), vec!["session.created"]);
    assert!(events[0].data.get("settingsJson").is_none());
    assert!(events[0].data.get("source").is_none());
}

#[test]
fn session_send_message_starts_run_and_streams_typed_sse_events() {
    let provider = successful_provider_with_text("hello from the frontend");
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );

    let create_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(1),
            "method": "session.create",
            "params": { "cwd": "/tmp/nav-workspace" }
        })
        .to_string(),
    ));

    assert_eq!(create_response.status(), 200);
    let create_body: Value = serde_json::from_str(create_response.body()).unwrap();
    let session_id = create_body["result"]["sessionId"]
        .as_str()
        .expect("session.create should return a session id");
    assert_uuid_v7(session_id);

    let send_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(2),
            "method": "session.sendMessage",
            "params": { "sessionId": session_id, "text": "hello from the frontend" }
        })
        .to_string(),
    ));

    assert_eq!(send_response.status(), 200);
    let send_body: Value = serde_json::from_str(send_response.body()).unwrap();
    let run_id = send_body["result"]["runId"]
        .as_str()
        .expect("session.sendMessage should return a run id");
    let message_id = send_body["result"]["messageId"]
        .as_str()
        .expect("session.sendMessage should return a message id");
    assert_uuid_v7(run_id);
    assert_uuid_v7(message_id);
    assert_eq!(provider.request().path, "/v1/chat/completions");
    wait_for_run_status(&server, run_id, RunStatus::Completed);

    let event_response =
        server.handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")));
    assert_eq!(event_response.status(), 200);
    assert_eq!(event_response.content_type(), "text/event-stream");

    let events = parse_sse(event_response.body());
    assert_eq!(
        event_names(&events),
        vec![
            "session.created",
            "run.started",
            "model.text_delta",
            "message.completed",
            "run.completed",
        ]
    );
    assert_protocol_event_ids(&events, session_id);

    let text_delta = events
        .iter()
        .find(|event| event.name == "model.text_delta")
        .expect("run should expose model text over SSE");
    assert_uuid_v7(&text_delta.id);
    assert_eq!(text_delta.data["session_id"], session_id);
    assert_eq!(text_delta.data["run_id"], run_id);
    assert_eq!(text_delta.data["message_id"], message_id);
    assert_eq!(text_delta.data["delta"], "hello from the frontend");
    assert_eq!(
        text_delta.data["metadata"]["provider_id"],
        "compatible-gateway"
    );
    assert_eq!(
        text_delta.data["metadata"]["configured_model_id"],
        "vendor/model-large"
    );

    let replay_response = server.handle_request(
        HttpRequest::get(format!("/sessions/{session_id}/events"))
            .with_last_event_id(events[1].id.clone()),
    );
    let replayed_events = parse_sse(replay_response.body());
    assert_eq!(
        event_names(&replayed_events),
        vec!["model.text_delta", "message.completed", "run.completed"]
    );
    assert_protocol_event_ids(&replayed_events, session_id);
}

#[test]
fn session_send_message_posts_provider_stream_and_publishes_provider_events() {
    let provider = FakeProviderServer::start(
        200,
        "text/event-stream",
        vec![
            provider_sse_chunk(
                r#"{"id":"provider-run","model":"vendor/model-large","choices":[{"index":0,"delta":{"content":"hello "},"finish_reason":null}]}"#,
            ),
            provider_sse_chunk(
                r#"{"id":"provider-run","model":"vendor/model-large","choices":[{"index":0,"delta":{"content":"Season"},"finish_reason":null}]}"#,
            ),
            provider_sse_chunk(
                r#"{"id":"provider-run","model":"vendor/model-large","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            ),
            "data: [DONE]\n\n".to_string(),
        ],
    );
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session(&mut server);

    let run_id = send_message(&mut server, &session_id);

    let request = provider.request();
    wait_for_run_status(&server, &run_id, RunStatus::Completed);
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
    assert_eq!(request.body["stream"], true);
    assert_eq!(request.body["messages"][0]["role"], "user");
    assert_eq!(request.body["messages"][0]["content"], "hello");

    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );
    assert_eq!(
        event_names(&events),
        vec![
            "session.created",
            "run.started",
            "model.text_delta",
            "model.text_delta",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[2].data["delta"], "hello ");
    assert_eq!(events[3].data["delta"], "Season");
    assert_eq!(
        events[2].data["metadata"]["provider_response_id"],
        "provider-run"
    );
    assert_eq!(
        events[2].data["metadata"]["provider_model"],
        "vendor/model-large"
    );
    assert_eq!(
        server.run_status(&RunId::try_new(run_id).unwrap()),
        Some(RunStatus::Completed)
    );
}

#[test]
fn session_send_message_publishes_provider_error_before_run_failed() {
    let provider = FakeProviderServer::start(
        429,
        "application/json",
        vec![
            r#"{"error":{"message":"rate limit exceeded","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#
                .to_string(),
        ],
    );
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session(&mut server);

    let run_id = send_message(&mut server, &session_id);

    assert_eq!(provider.request().path, "/v1/chat/completions");
    wait_for_run_status(&server, &run_id, RunStatus::Failed);
    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );
    assert_eq!(
        event_names(&events),
        vec![
            "session.created",
            "run.started",
            "provider.error",
            "run.failed",
        ]
    );
    let provider_error = &events[2].data;
    assert_eq!(provider_error["run_id"], run_id);
    assert_eq!(provider_error["status"], 429);
    assert_eq!(provider_error["message"], "rate limit exceeded");
    assert_eq!(provider_error["error_type"], "rate_limit_error");
    assert_eq!(provider_error["code"], "rate_limit_exceeded");
    assert_eq!(
        provider_error["metadata"]["provider_id"],
        "compatible-gateway"
    );

    let failed = &events[3].data;
    assert_eq!(failed["run_id"], run_id);
    assert_eq!(failed["message"], "provider error: rate limit exceeded");
    assert_eq!(
        server.run_status(&RunId::try_new(run_id).unwrap()),
        Some(RunStatus::Failed)
    );
}

#[test]
fn session_send_message_marks_transport_failure_as_run_failed() {
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(unused_local_base_url()),
    );
    let session_id = create_session(&mut server);

    let run_id = send_message(&mut server, &session_id);
    wait_for_run_status(&server, &run_id, RunStatus::Failed);

    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );
    assert_eq!(
        event_names(&events),
        vec!["session.created", "run.started", "run.failed"]
    );
    let failed = &events[2].data;
    assert_eq!(failed["run_id"], run_id);
    assert!(
        failed["message"]
            .as_str()
            .unwrap()
            .contains("request transport failed")
    );
    assert_eq!(
        server.run_status(&RunId::try_new(run_id).unwrap()),
        Some(RunStatus::Failed)
    );
}

#[test]
fn run_cancel_cancels_active_provider_stream_and_publishes_run_cancelled() {
    let provider = HangingProviderServer::start();
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session(&mut server);
    let run_id = send_message(&mut server, &session_id);

    let request = provider.wait_for_request();
    assert_eq!(request.path, "/v1/chat/completions");
    assert_eq!(
        server.run_status(&RunId::try_new(&run_id).unwrap()),
        Some(RunStatus::Running)
    );

    let cancel_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(7),
            "method": "run.cancel",
            "params": { "runId": run_id }
        })
        .to_string(),
    ));
    let cancel_body: Value = serde_json::from_str(cancel_response.body()).unwrap();
    assert_eq!(cancel_body["result"]["runId"], run_id);
    wait_for_run_status(&server, &run_id, RunStatus::Cancelled);
    provider.stop();

    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );
    assert_eq!(
        event_names(&events),
        vec!["session.created", "run.started", "run.cancelled"]
    );
    assert_eq!(events[2].data["run_id"], run_id);
}

#[test]
fn session_events_returns_conflict_for_unknown_last_event_id() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());
    let session_id = create_session(&mut server);

    let response = server.handle_request(
        HttpRequest::get(format!("/sessions/{session_id}/events"))
            .with_last_event_id(request_id(999)),
    );

    assert_eq!(response.status(), 409);
    assert!(response.body().contains("is not retained for this session"));
}

#[test]
fn session_event_subscriber_receives_events_appended_after_subscription() {
    let provider = successful_provider_with_text("hello");
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = SessionId::try_new(create_session(&mut server)).unwrap();
    let subscription = server
        .subscribe_session_events(&session_id, None)
        .expect("session event subscription should open");

    assert_eq!(
        envelope_event_names(subscription.replay()),
        vec!["session.created"]
    );

    let run_id = send_message(&mut server, session_id.as_str());
    assert_eq!(provider.request().path, "/v1/chat/completions");
    let live_events = receive_live_events(&subscription, 4);

    assert_eq!(
        envelope_event_names(&live_events),
        vec![
            "run.started",
            "model.text_delta",
            "message.completed",
            "run.completed",
        ]
    );
    assert!(
        live_events
            .iter()
            .all(|event| event.session_id == session_id)
    );
    wait_for_run_status(&server, &run_id, RunStatus::Completed);
    assert_eq!(
        server.run_status(&RunId::try_new(run_id).unwrap()),
        Some(RunStatus::Completed)
    );
    assert!(subscription.try_recv().is_err());
}

#[test]
fn run_status_transitions_are_explicit() {
    let provider = successful_provider_with_text("hello");
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session(&mut server);
    let completed_run_id = RunId::try_new(send_message(&mut server, &session_id))
        .expect("completed run id should be valid");
    assert_eq!(provider.request().path, "/v1/chat/completions");
    wait_for_run_status(&server, completed_run_id.as_str(), RunStatus::Completed);

    assert_eq!(
        server.run_status(&completed_run_id),
        Some(RunStatus::Completed)
    );

    let mut failing_server =
        HttpServer::with_model_settings(HttpServerConfig::default(), missing_key_model_settings());
    let failing_session_id = create_session(&mut failing_server);
    let failed_run_id = RunId::try_new(send_message(&mut failing_server, &failing_session_id))
        .expect("failed run id should be valid");
    wait_for_run_status(&failing_server, failed_run_id.as_str(), RunStatus::Failed);

    assert_eq!(
        failing_server.run_status(&failed_run_id),
        Some(RunStatus::Failed)
    );
}

#[test]
fn session_send_message_rejects_blank_text_without_starting_a_run() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());
    let session_id = create_session(&mut server);

    let send_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(3),
            "method": "session.sendMessage",
            "params": { "sessionId": session_id, "text": "   " }
        })
        .to_string(),
    ));

    assert_eq!(send_response.status(), 200);
    let send_body: Value = serde_json::from_str(send_response.body()).unwrap();
    assert_eq!(send_body["error"]["code"], -32602);
    assert!(
        send_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("text is required")
    );

    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );
    assert_eq!(event_names(&events), vec!["session.created"]);
}

#[test]
fn session_send_message_streams_run_failed_when_default_model_cannot_resolve() {
    let mut server =
        HttpServer::with_model_settings(HttpServerConfig::default(), missing_key_model_settings());
    let session_id = create_session(&mut server);

    let send_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(4),
            "method": "session.sendMessage",
            "params": { "sessionId": session_id, "text": "try a model run" }
        })
        .to_string(),
    ));

    assert_eq!(send_response.status(), 200);
    let send_body: Value = serde_json::from_str(send_response.body()).unwrap();
    let run_id = send_body["result"]["runId"].as_str().unwrap();
    assert_uuid_v7(run_id);
    wait_for_run_status(&server, run_id, RunStatus::Failed);

    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );
    assert_eq!(
        event_names(&events),
        vec!["session.created", "run.started", "run.failed"]
    );

    let failed = events
        .iter()
        .find(|event| event.name == "run.failed")
        .expect("failed run should be exposed over SSE");
    assert_eq!(failed.data["run_id"], run_id);
    assert!(
        failed.data["message"]
            .as_str()
            .unwrap()
            .contains("MissingApiKey")
    );

    let cancel_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(6),
            "method": "run.cancel",
            "params": { "runId": run_id }
        })
        .to_string(),
    ));
    let cancel_body: Value = serde_json::from_str(cancel_response.body()).unwrap();
    assert_eq!(cancel_body["error"]["code"], -32005);
    assert!(
        cancel_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already failed")
    );
}

#[test]
fn run_cancel_rejects_completed_runs_without_adding_events() {
    let provider = successful_provider_with_text("hello");
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session(&mut server);
    let run_id = send_message(&mut server, &session_id);
    assert_eq!(provider.request().path, "/v1/chat/completions");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);
    let initial_events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );

    let cancel_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(5),
            "method": "run.cancel",
            "params": { "runId": run_id }
        })
        .to_string(),
    ));

    assert_eq!(cancel_response.status(), 200);
    let cancel_body: Value = serde_json::from_str(cancel_response.body()).unwrap();
    assert_eq!(cancel_body["error"]["code"], -32005);
    assert!(
        cancel_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already completed")
    );

    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );
    assert_eq!(event_names(&events), event_names(&initial_events));
}

#[derive(Debug)]
struct SseEvent {
    id: String,
    name: String,
    data: Value,
}

fn parse_sse(body: &str) -> Vec<SseEvent> {
    body.split("\n\n")
        .filter(|chunk| !chunk.trim().is_empty())
        .map(|chunk| {
            let mut id = None;
            let mut name = None;
            let mut data = None;

            for line in chunk.lines() {
                if let Some(value) = line.strip_prefix("id: ") {
                    id = Some(value.to_string());
                } else if let Some(value) = line.strip_prefix("event: ") {
                    name = Some(value.to_string());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data = Some(serde_json::from_str(value).unwrap());
                }
            }

            SseEvent {
                id: id.expect("SSE event should include an id"),
                name: name.expect("SSE event should include an event name"),
                data: data.expect("SSE event should include JSON data"),
            }
        })
        .collect()
}

fn event_names(events: &[SseEvent]) -> Vec<&str> {
    events.iter().map(|event| event.name.as_str()).collect()
}

fn assert_protocol_event_ids(events: &[SseEvent], session_id: &str) {
    for event in events {
        assert_uuid_v7(&event.id);
        assert_eq!(event.data["event_id"], event.id);
        assert_eq!(event.data["session_id"], session_id);
    }
}

fn receive_live_events(
    subscription: &ProtocolEventSubscription,
    count: usize,
) -> Vec<EventEnvelope> {
    (0..count)
        .map(|_| {
            subscription
                .recv_timeout(LIVE_EVENT_TIMEOUT)
                .expect("live subscription should receive appended event")
        })
        .collect()
}

fn envelope_event_names(events: &[EventEnvelope]) -> Vec<&str> {
    events.iter().map(|event| event.event_type()).collect()
}

fn create_session(server: &mut HttpServer) -> String {
    let create_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(99),
            "method": "session.create",
            "params": { "cwd": "/tmp/nav-workspace" }
        })
        .to_string(),
    ));
    let create_body: Value = serde_json::from_str(create_response.body()).unwrap();
    create_body["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string()
}

fn send_message(server: &mut HttpServer, session_id: &str) -> String {
    let send_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(98),
            "method": "session.sendMessage",
            "params": { "sessionId": session_id, "text": "hello" }
        })
        .to_string(),
    ));
    let send_body: Value = serde_json::from_str(send_response.body()).unwrap();
    send_body["result"]["runId"].as_str().unwrap().to_string()
}

fn model_settings() -> ModelSettings {
    model_settings_with_base_url("https://gateway.example.com/v1".to_string())
}

fn model_settings_with_base_url(base_url: String) -> ModelSettings {
    let mut settings = ModelSettings {
        default_model: Some(ModelRef {
            provider: "compatible-gateway".to_string(),
            model: "vendor/model-large".to_string(),
        }),
        ..ModelSettings::default()
    };

    settings.providers.insert(
        "compatible-gateway".to_string(),
        ProviderConfig {
            name: Some("Compatible Gateway".to_string()),
            api: ApiKind::OpenAiCompletions,
            base_url,
            api_key: ApiKeyConfig::Inline {
                inline: "sk-test".to_string(),
            },
            models: vec![ModelConfig {
                id: "vendor/model-large".to_string(),
                name: None,
                api: None,
                base_url: None,
                reasoning: false,
                input: vec![ModelInput::Text],
                context_window: None,
                max_tokens: None,
                compat: ProviderCompat::default(),
            }],
            compat: ProviderCompat::default(),
        },
    );

    settings
}

fn missing_key_model_settings() -> ModelSettings {
    let mut settings = model_settings();
    let provider = settings
        .providers
        .get_mut("compatible-gateway")
        .expect("fixture should include provider");
    let missing_env_var = (0u32..)
        .map(|index| format!("NAV_TEST_MISSING_API_KEY_{}_{}", std::process::id(), index))
        .find(|name| std::env::var_os(name).is_none())
        .expect("should find an unset env var name for test fixture");
    provider.api_key = ApiKeyConfig::EnvVar {
        env_var: missing_env_var,
    };
    settings
}

fn request_id(index: u64) -> String {
    format!("019f2f6f-f178-7a72-9f28-{index:012x}")
}

fn assert_uuid_v7(value: &str) {
    assert_eq!(value.len(), 36);
    assert_eq!(&value[14..15], "7");
    assert!(matches!(&value[19..20], "8" | "9" | "a" | "b"));
}
