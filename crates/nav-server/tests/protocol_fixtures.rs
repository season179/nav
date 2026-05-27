use std::path::{Path, PathBuf};

use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelInput, ModelRef, ModelSettings, ProviderCompat,
    ProviderConfig,
};
use nav_protocol::{EventEnvelope, JsonRpcRequest, JsonRpcResponse};
use nav_server::http::{HttpRequest, HttpServer, HttpServerConfig, RunStatus, sse};
use serde_json::{Value, json};

mod support;

use support::{successful_provider_with_text, wait_for_run_status};

const REQUEST_FIXTURES: &[(&str, &str)] = &[
    ("json-rpc/initialize-request.json", "initialize"),
    ("json-rpc/session-create-request.json", "session.create"),
    (
        "json-rpc/session-send-message-request.json",
        "session.sendMessage",
    ),
];

const RESPONSE_FIXTURES: &[&str] = &[
    "json-rpc/initialize-response.json",
    "json-rpc/session-create-response.json",
    "json-rpc/session-send-message-response.json",
];

const SSE_FIXTURES: &[&str] = &[
    "event-streams/session-created.sse",
    "event-streams/message-send-completed.sse",
    "event-streams/replay-after-run-started.sse",
    "event-streams/run-failed.sse",
    "event-streams/provider-error.sse",
    "event-streams/tool-call-read.sse",
    "event-streams/tool-call-failed.sse",
];

#[test]
fn json_rpc_fixtures_are_valid_protocol_envelopes() {
    for &(fixture, expected_method) in REQUEST_FIXTURES {
        let value = fixture_json(fixture);
        let request: JsonRpcRequest<Value> = serde_json::from_value(value.clone())
            .unwrap_or_else(|error| panic!("{fixture} should parse as a request: {error}"));

        assert_eq!(value["jsonrpc"].as_str(), Some("2.0"), "{fixture}");
        assert_uuid_v7(request.id.as_str());
        assert_eq!(request.method, expected_method);
    }

    let initialize = fixture_json("json-rpc/initialize-request.json");
    assert_eq!(initialize["params"]["clientName"].as_str(), Some("nav-web"));
    assert_eq!(initialize["params"]["clientKind"].as_str(), Some("web"));

    for fixture in RESPONSE_FIXTURES {
        let value = fixture_json(fixture);
        let response: JsonRpcResponse<Value> = serde_json::from_value(value.clone())
            .unwrap_or_else(|error| panic!("{fixture} should parse as a response: {error}"));

        assert_eq!(value["jsonrpc"].as_str(), Some("2.0"), "{fixture}");
        assert_uuid_v7(response.id.as_str());
        assert!(
            response.result.is_some() ^ response.error.is_some(),
            "{fixture} should contain exactly one of result or error"
        );
    }
}

#[test]
fn sse_fixtures_are_valid_typed_protocol_events() {
    let mut saw_provider_error = false;
    let mut saw_tool_call_started = false;
    let mut saw_tool_call_completed = false;
    let mut saw_tool_call_failed = false;

    for fixture in SSE_FIXTURES {
        let events = parse_sse(&fixture_text(fixture));
        assert!(!events.is_empty(), "{fixture} should contain events");

        for event in events {
            assert_uuid_v7(&event.id);
            assert_eq!(event.data["event_id"].as_str(), Some(event.id.as_str()));
            assert_eq!(event.data["type"].as_str(), Some(event.name.as_str()));

            let envelope: EventEnvelope = serde_json::from_value(event.data.clone())
                .unwrap_or_else(|error| panic!("{fixture} should parse as EventEnvelope: {error}"));
            assert_eq!(envelope.event_id.as_str(), event.id);
            assert_eq!(envelope.event_type(), event.name);

            match event.name.as_str() {
                "provider.error" => saw_provider_error = true,
                "tool.call_started" => saw_tool_call_started = true,
                "tool.call_completed" => saw_tool_call_completed = true,
                "tool.call_failed" => saw_tool_call_failed = true,
                _ => {}
            }
        }
    }

    assert!(saw_provider_error, "fixtures should cover provider.error");
    assert!(saw_tool_call_started, "fixtures should cover tool.call_started");
    assert!(saw_tool_call_completed, "fixtures should cover tool.call_completed");
    assert!(saw_tool_call_failed, "fixtures should cover tool.call_failed");
}

#[test]
fn sse_fixtures_match_server_encoder() {
    for fixture in SSE_FIXTURES {
        let fixture_body = fixture_text(fixture);
        let events = parse_sse(&fixture_body);
        let envelopes = event_envelopes(events);
        let encoded = sse::encode_events(&envelopes)
            .unwrap_or_else(|error| panic!("{fixture} should encode as SSE: {error}"));

        assert_eq!(
            encoded,
            fixture_body_with_final_separator(&fixture_body),
            "{fixture}"
        );
    }
}

#[test]
fn initialize_fixture_matches_server_contract() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());
    let request_body = fixture_text("json-rpc/initialize-request.json");
    let expected = fixture_json("json-rpc/initialize-response.json");

    let response = server.handle_request(HttpRequest::post("/rpc", request_body));

    assert_eq!(response.status(), 200);
    assert_eq!(response.content_type(), "application/json");
    let actual: Value = serde_json::from_str(response.body()).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn session_create_fixture_matches_server_contract() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());
    let request_body = fixture_text("json-rpc/session-create-request.json");
    let request: JsonRpcRequest<Value> = serde_json::from_str(&request_body).unwrap();
    let expected: JsonRpcResponse<Value> =
        fixture_json_response("json-rpc/session-create-response.json");

    let response = server.handle_request(HttpRequest::post("/rpc", request_body));

    assert_eq!(response.status(), 200);
    assert_eq!(response.content_type(), "application/json");
    let actual: Value = serde_json::from_str(response.body()).unwrap();
    assert_eq!(actual["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(actual["id"].as_str(), Some(request.id.as_str()));
    assert_eq!(actual["id"].as_str(), Some(expected.id.as_str()));
    assert!(actual.get("error").is_none());

    let session_id = actual["result"]["sessionId"]
        .as_str()
        .expect("session.create should return a sessionId");
    assert_uuid_v7(session_id);

    let events = session_events(&mut server, session_id);
    assert_eq!(
        event_names(&events),
        fixture_event_names("event-streams/session-created.sse")
    );
    assert_protocol_event_ids(&events, session_id);
}

#[test]
fn session_send_message_fixture_matches_server_contract_and_replay() {
    let mut request = fixture_json("json-rpc/session-send-message-request.json");
    let expected: JsonRpcResponse<Value> =
        fixture_json_response("json-rpc/session-send-message-response.json");
    let request_id = request["id"].as_str().unwrap().to_string();
    let request_text = request["params"]["text"].as_str().unwrap().to_string();
    let provider = successful_provider_with_text(&request_text);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session_from_fixture(&mut server);
    request["params"]["sessionId"] = json!(session_id);

    let response = server.handle_request(HttpRequest::post("/rpc", request.to_string()));

    assert_eq!(response.status(), 200);
    let actual: Value = serde_json::from_str(response.body()).unwrap();
    assert_eq!(actual["id"].as_str(), Some(request_id.as_str()));
    assert_eq!(expected.id.as_str(), request_id);
    assert_eq!(
        actual["result"]["sessionId"].as_str(),
        Some(session_id.as_str())
    );
    let run_id = actual["result"]["runId"].as_str().unwrap();
    let message_id = actual["result"]["messageId"].as_str().unwrap();
    assert_uuid_v7(run_id);
    assert_uuid_v7(message_id);
    let provider_request = provider.request();
    assert_eq!(provider_request.path, "/v1/chat/completions");
    assert_eq!(
        provider_request.body["messages"][0]["content"],
        request_text
    );
    wait_for_run_status(&server, run_id, RunStatus::Completed);

    let events = session_events(&mut server, &session_id);
    assert_eq!(
        event_names(&events),
        fixture_event_names("event-streams/message-send-completed.sse")
    );
    assert_protocol_event_ids(&events, &session_id);
    assert_eq!(events[1].data["run_id"].as_str(), Some(run_id));
    assert_eq!(events[2].data["run_id"].as_str(), Some(run_id));
    assert_eq!(events[2].data["message_id"].as_str(), Some(message_id));
    assert_eq!(
        events[2].data["delta"].as_str(),
        Some(request_text.as_str())
    );
    assert_eq!(
        events[2].data["metadata"]["provider_id"].as_str(),
        Some("compatible-gateway")
    );
    assert_eq!(
        events[2].data["metadata"]["configured_model_id"].as_str(),
        Some("vendor/model-large")
    );

    let replay_response = server.handle_request(
        HttpRequest::get(format!("/sessions/{session_id}/events"))
            .with_last_event_id(events[1].id.clone()),
    );
    let replayed_events = parse_sse(replay_response.body());
    assert_eq!(
        event_names(&replayed_events),
        fixture_event_names("event-streams/replay-after-run-started.sse")
    );
    assert_protocol_event_ids(&replayed_events, &session_id);
}

#[test]
fn run_failed_fixture_matches_server_contract() {
    let mut server =
        HttpServer::with_model_settings(HttpServerConfig::default(), missing_key_model_settings());
    let session_id = create_session_from_fixture(&mut server);

    let mut request = fixture_json("json-rpc/session-send-message-request.json");
    request["params"]["sessionId"] = json!(session_id);
    let response = server.handle_request(HttpRequest::post("/rpc", request.to_string()));
    assert_eq!(response.status(), 200);
    let body: Value = serde_json::from_str(response.body()).unwrap();
    let run_id = body["result"]["runId"].as_str().unwrap();
    assert_uuid_v7(run_id);
    wait_for_run_status(&server, run_id, RunStatus::Failed);

    let events = session_events(&mut server, &session_id);
    assert_eq!(
        event_names(&events),
        fixture_event_names("event-streams/run-failed.sse")
    );
    assert_protocol_event_ids(&events, &session_id);
    assert_eq!(events[1].data["run_id"].as_str(), Some(run_id));
    assert_eq!(events[2].data["run_id"].as_str(), Some(run_id));
    assert!(
        events[2].data["message"]
            .as_str()
            .unwrap()
            .contains("MissingApiKey")
    );
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

fn fixture_event_names(relative_path: &str) -> Vec<String> {
    event_names(&parse_sse(&fixture_text(relative_path)))
}

fn event_names(events: &[SseEvent]) -> Vec<String> {
    events.iter().map(|event| event.name.clone()).collect()
}

fn event_envelopes(events: Vec<SseEvent>) -> Vec<EventEnvelope> {
    events
        .into_iter()
        .map(|event| {
            serde_json::from_value(event.data)
                .unwrap_or_else(|error| panic!("SSE data should parse as EventEnvelope: {error}"))
        })
        .collect()
}

fn assert_protocol_event_ids(events: &[SseEvent], session_id: &str) {
    for event in events {
        assert_uuid_v7(&event.id);
        assert_eq!(event.data["event_id"].as_str(), Some(event.id.as_str()));
        assert_eq!(event.data["session_id"].as_str(), Some(session_id));
    }
}

fn create_session_from_fixture(server: &mut HttpServer) -> String {
    let response = server.handle_request(HttpRequest::post(
        "/rpc",
        fixture_text("json-rpc/session-create-request.json"),
    ));
    let body: Value = serde_json::from_str(response.body()).unwrap();
    body["result"]["sessionId"].as_str().unwrap().to_string()
}

fn session_events(server: &mut HttpServer, session_id: &str) -> Vec<SseEvent> {
    parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    )
}

fn fixture_json_response(relative_path: &str) -> JsonRpcResponse<Value> {
    serde_json::from_value(fixture_json(relative_path)).unwrap()
}

fn fixture_json(relative_path: &str) -> Value {
    serde_json::from_str(&fixture_text(relative_path))
        .unwrap_or_else(|error| panic!("{relative_path} should be valid JSON: {error}"))
}

fn fixture_text(relative_path: &str) -> String {
    std::fs::read_to_string(fixture_path(relative_path))
        .unwrap_or_else(|error| panic!("{relative_path} should be readable: {error}"))
}

fn fixture_body_with_final_separator(body: &str) -> String {
    if body.ends_with("\n\n") {
        return body.to_string();
    }

    if body.ends_with('\n') {
        return format!("{body}\n");
    }

    format!("{body}\n\n")
}

fn fixture_path(relative_path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/protocol")
        .join(relative_path)
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

fn assert_uuid_v7(value: &str) {
    assert_eq!(value.len(), 36);
    assert_eq!(&value[14..15], "7");
    assert!(matches!(&value[19..20], "8" | "9" | "a" | "b"));
}

#[test]
fn tool_call_read_fixture_round_trips_through_sse_encoder() {
    let fixture = "event-streams/tool-call-read.sse";
    let fixture_body = fixture_text(fixture);
    let events = parse_sse(&fixture_body);

    let event_sequence: Vec<&str> = events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        event_sequence,
        vec![
            "session.created",
            "run.started",
            "tool.call_started",
            "tool.call_delta",
            "tool.call_completed",
            "message.completed",
            "model.text_delta",
            "message.completed",
            "run.completed",
        ]
    );

    let envelopes = event_envelopes(events);
    let encoded = sse::encode_events(&envelopes)
        .unwrap_or_else(|error| panic!("{fixture} should encode: {error}"));
    assert_eq!(encoded, fixture_body_with_final_separator(&fixture_body));

    let started = envelopes.iter().find(|e| e.event_type() == "tool.call_started").unwrap();
    let completed = envelopes.iter().find(|e| e.event_type() == "tool.call_completed").unwrap();
    let delta = envelopes.iter().find(|e| e.event_type() == "tool.call_delta").unwrap();

    // Verify tool call events have consistent run_id and tool_call_id
    match (&started.event, &completed.event, &delta.event) {
        (
            nav_protocol::BackendEvent::ToolCallStarted { run_id: rid1, tool_call_id: tcid1, name, .. },
            nav_protocol::BackendEvent::ToolCallCompleted { run_id: rid2, tool_call_id: tcid2, .. },
            nav_protocol::BackendEvent::ToolCallDelta { run_id: rid3, tool_call_id: tcid3, .. },
        ) => {
            assert_eq!(rid1, rid2);
            assert_eq!(rid1, rid3);
            assert_eq!(tcid1, tcid2);
            assert_eq!(tcid1, tcid3);
            assert_eq!(name.as_deref(), Some("read"));
        }
        _ => panic!("unexpected event types"),
    }
}

#[test]
fn tool_call_failed_fixture_round_trips_through_sse_encoder() {
    let fixture = "event-streams/tool-call-failed.sse";
    let fixture_body = fixture_text(fixture);
    let events = parse_sse(&fixture_body);

    let event_sequence: Vec<&str> = events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        event_sequence,
        vec![
            "session.created",
            "run.started",
            "tool.call_started",
            "tool.call_delta",
            "tool.call_completed",
            "message.completed",
            "tool.call_failed",
            "model.text_delta",
            "message.completed",
            "run.completed",
        ]
    );

    let envelopes = event_envelopes(events);
    let encoded = sse::encode_events(&envelopes)
        .unwrap_or_else(|error| panic!("{fixture} should encode: {error}"));
    assert_eq!(encoded, fixture_body_with_final_separator(&fixture_body));

    let failed = envelopes.iter().find(|e| e.event_type() == "tool.call_failed").unwrap();
    match &failed.event {
        nav_protocol::BackendEvent::ToolCallFailed { name, error_message, .. } => {
            assert_eq!(name.as_deref(), Some("read"));
            assert!(!error_message.is_empty());
        }
        _ => panic!("expected tool.call_failed"),
    }
}
