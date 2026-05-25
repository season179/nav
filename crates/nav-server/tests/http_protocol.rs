use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelInput, ModelRef, ModelSettings, ProviderCompat,
    ProviderConfig,
};
use nav_server::http::{HttpRequest, HttpServer, HttpServerConfig};
use serde_json::{Value, json};

#[test]
fn session_send_message_starts_run_and_streams_typed_sse_events() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());

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
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());
    let session_id = create_session(&mut server);
    let run_id = send_message(&mut server, &session_id);
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
            base_url: "https://gateway.example.com/v1".to_string(),
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
    provider.api_key = ApiKeyConfig::EnvVar {
        env_var: "NAV_TEST_MISSING_API_KEY".to_string(),
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
