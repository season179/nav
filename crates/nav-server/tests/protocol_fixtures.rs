use std::path::{Path, PathBuf};
use std::time::Duration;

use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelInput, ModelRef, ModelSettings, ProviderCompat,
    ProviderConfig,
};
use nav_harness::sessions::{ConfirmationDecision, PendingConfirmation};
use nav_protocol::{EventEnvelope, JsonRpcRequest, JsonRpcResponse};
use nav_server::http::{HttpRequest, HttpServer, HttpServerConfig, RunStatus, sse};
use nav_types::{ApprovalId, FileChangeKind, PartId, RunId, ToolCallId};
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
    ("json-rpc/tool-approve-request.json", "tool.approve"),
    ("json-rpc/tool-reject-request.json", "tool.reject"),
];

const RESPONSE_FIXTURES: &[&str] = &[
    "json-rpc/initialize-response.json",
    "json-rpc/session-create-response.json",
    "json-rpc/session-send-message-response.json",
    "json-rpc/tool-approve-response.json",
    "json-rpc/tool-reject-response.json",
];

const SSE_FIXTURES: &[&str] = &[
    "event-streams/session-created.sse",
    "event-streams/message-send-completed.sse",
    "event-streams/replay-after-run-started.sse",
    "event-streams/run-failed.sse",
    "event-streams/provider-error.sse",
    "event-streams/tool-call-read.sse",
    "event-streams/tool-call-failed.sse",
    "event-streams/tool-approval-requested.sse",
    "event-streams/file-changed.sse",
    "event-streams/part-delta.sse",
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
    let mut saw_tool_approval_requested = false;
    let mut saw_part_delta = false;
    let mut saw_part_completed = false;

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
                "tool.approval_requested" => saw_tool_approval_requested = true,
                "part.delta" => saw_part_delta = true,
                "part.completed" => saw_part_completed = true,
                _ => {}
            }
        }
    }

    assert!(saw_provider_error, "fixtures should cover provider.error");
    assert!(
        saw_tool_call_started,
        "fixtures should cover tool.call_started"
    );
    assert!(
        saw_tool_call_completed,
        "fixtures should cover tool.call_completed"
    );
    assert!(
        saw_tool_call_failed,
        "fixtures should cover tool.call_failed"
    );
    assert!(
        saw_tool_approval_requested,
        "fixtures should cover tool.approval_requested"
    );
    assert!(saw_part_delta, "fixtures should cover part.delta");
    assert!(saw_part_completed, "fixtures should cover part.completed");
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
    // messages[0] is the assembled system prompt (issue #524); the user turn
    // follows it.
    assert_eq!(provider_request.body["messages"][0]["role"], "system");
    assert_eq!(
        provider_request.body["messages"][1]["content"],
        request_text
    );
    wait_for_run_status(&server, run_id, RunStatus::Completed);

    let events = wait_for_session_events(
        &mut server,
        &session_id,
        &fixture_event_names("event-streams/message-send-completed.sse"),
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
fn tool_approve_fixture_satisfies_pending_confirmation() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());
    let request_fixture = "json-rpc/tool-approve-request.json";
    let approval_id = fixture_approval_id(request_fixture);
    let receiver = server
        .register_pending_confirmation(pending_confirmation(approval_id))
        .expect("pending confirmation should register");

    let actual = rpc_fixture_response(&mut server, request_fixture);

    assert_eq!(actual, fixture_json("json-rpc/tool-approve-response.json"));
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_millis(100))
            .expect("approval decision should be delivered"),
        ConfirmationDecision::Approved
    );

    let already_resolved = rpc_fixture_response(&mut server, request_fixture);
    assert_not_pending_confirmation_error(&already_resolved);
}

#[test]
fn tool_reject_fixture_satisfies_pending_confirmation() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());
    let request_fixture = "json-rpc/tool-reject-request.json";
    let approval_id = fixture_approval_id(request_fixture);
    let receiver = server
        .register_pending_confirmation(pending_confirmation(approval_id))
        .expect("pending confirmation should register");

    let actual = rpc_fixture_response(&mut server, request_fixture);

    assert_eq!(actual, fixture_json("json-rpc/tool-reject-response.json"));
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_millis(100))
            .expect("rejection decision should be delivered"),
        ConfirmationDecision::Rejected {
            reason: Some("user declined the tool request".to_string())
        }
    );
}

#[test]
fn tool_approve_returns_structured_error_for_unknown_confirmation() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());
    let body = rpc_fixture_response(&mut server, "json-rpc/tool-approve-request.json");

    assert_not_pending_confirmation_error(&body);
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

    let events = wait_for_session_events(
        &mut server,
        &session_id,
        &fixture_event_names("event-streams/run-failed.sse"),
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

/// Fetches the session event stream, polling until the event names match
/// `expected` (or a short deadline elapses) before returning the parsed events.
///
/// The run status (observed by `wait_for_run_status`) flips to its terminal
/// value before the trailing `session.totals_updated` event is appended by the
/// run thread, so under heavy parallel load a reader can snapshot the stream in
/// the window before that event lands. Polling closes that race; if the events
/// never settle we still assert against the last snapshot for a useful diff.
fn wait_for_session_events(
    server: &mut HttpServer,
    session_id: &str,
    expected: &[String],
) -> Vec<SseEvent> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let events = session_events(server, session_id);
        if event_names(&events) == expected || std::time::Instant::now() >= deadline {
            assert_eq!(event_names(&events), expected);
            return events;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fixture_json_response(relative_path: &str) -> JsonRpcResponse<Value> {
    serde_json::from_value(fixture_json(relative_path)).unwrap()
}

fn fixture_json(relative_path: &str) -> Value {
    serde_json::from_str(&fixture_text(relative_path))
        .unwrap_or_else(|error| panic!("{relative_path} should be valid JSON: {error}"))
}

fn rpc_fixture_response(server: &mut HttpServer, request_fixture: &str) -> Value {
    let response = server.handle_request(HttpRequest::post("/rpc", fixture_text(request_fixture)));
    assert_eq!(response.status(), 200);
    assert_eq!(response.content_type(), "application/json");
    serde_json::from_str(response.body())
        .unwrap_or_else(|error| panic!("{request_fixture} response should be JSON: {error}"))
}

fn fixture_approval_id(request_fixture: &str) -> ApprovalId {
    let request = fixture_json(request_fixture);
    ApprovalId::try_new(
        request["params"]["approval_id"]
            .as_str()
            .expect("fixture should include approval_id"),
    )
    .expect("fixture approval_id should be valid")
}

fn assert_not_pending_confirmation_error(body: &Value) {
    assert!(body.get("result").is_none());
    assert_eq!(body["error"]["code"].as_i64(), Some(-32006));
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("error message should be present")
            .contains("not pending")
    );
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

fn pending_confirmation(approval_id: ApprovalId) -> PendingConfirmation {
    PendingConfirmation {
        approval_id,
        run_id: RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap(),
        tool_call_id: ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap(),
        tool_name: "write_file".to_string(),
        reason: "writes outside the current task focus".to_string(),
        arguments_summary: r#"{"path":"notes.md","content":"hello"}"#.to_string(),
        risk_class: Some("mutate".to_string()),
    }
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

    let started = envelopes
        .iter()
        .find(|e| e.event_type() == "tool.call_started")
        .unwrap();
    let completed = envelopes
        .iter()
        .find(|e| e.event_type() == "tool.call_completed")
        .unwrap();
    let delta = envelopes
        .iter()
        .find(|e| e.event_type() == "tool.call_delta")
        .unwrap();

    // Verify tool call events have consistent run_id and tool_call_id
    match (&started.event, &completed.event, &delta.event) {
        (
            nav_protocol::BackendEvent::ToolCallStarted {
                run_id: rid1,
                tool_call_id: tcid1,
                name,
                ..
            },
            nav_protocol::BackendEvent::ToolCallCompleted {
                run_id: rid2,
                tool_call_id: tcid2,
                ..
            },
            nav_protocol::BackendEvent::ToolCallDelta {
                run_id: rid3,
                tool_call_id: tcid3,
                ..
            },
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
fn file_changed_fixture_round_trips_every_kind() {
    let fixture = "event-streams/file-changed.sse";
    let fixture_body = fixture_text(fixture);
    let events = parse_sse(&fixture_body);

    let kinds: Vec<&str> = events
        .iter()
        .filter(|event| event.name == "file.changed")
        .map(|event| {
            event.data["kind"]
                .as_str()
                .expect("kind should be a string")
        })
        .collect();
    assert_eq!(kinds, vec!["created", "modified", "deleted"]);

    let envelopes = event_envelopes(events);
    let encoded = sse::encode_events(&envelopes)
        .unwrap_or_else(|error| panic!("{fixture} should encode: {error}"));
    assert_eq!(encoded, fixture_body_with_final_separator(&fixture_body));

    let decoded_kinds: Vec<FileChangeKind> = envelopes
        .iter()
        .filter_map(|envelope| match &envelope.event {
            nav_protocol::BackendEvent::FileChanged { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert_eq!(
        decoded_kinds,
        vec![
            FileChangeKind::Created,
            FileChangeKind::Modified,
            FileChangeKind::Deleted,
        ]
    );
}

#[test]
fn part_delta_fixture_round_trips_through_sse_encoder() {
    let fixture = "event-streams/part-delta.sse";
    let fixture_body = fixture_text(fixture);
    let events = parse_sse(&fixture_body);

    let event_sequence: Vec<&str> = events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        event_sequence,
        vec![
            "session.created",
            "part.delta",
            "part.delta",
            "part.delta",
            "part.completed",
        ]
    );

    let envelopes = event_envelopes(events);
    let encoded = sse::encode_events(&envelopes)
        .unwrap_or_else(|error| panic!("{fixture} should encode: {error}"));
    assert_eq!(encoded, fixture_body_with_final_separator(&fixture_body));

    let deltas: Vec<_> = envelopes
        .iter()
        .filter_map(|envelope| match &envelope.event {
            nav_protocol::BackendEvent::PartDelta {
                turn_id,
                part_id,
                field,
                delta,
            } => Some((turn_id, part_id, field.as_str(), delta.as_str())),
            _ => None,
        })
        .collect();

    assert_eq!(deltas.len(), 3);
    assert_eq!(deltas[0].2, "text");
    assert_eq!(deltas[0].3, "hello");
    assert_eq!(deltas[1].2, "text");
    assert_eq!(deltas[1].3, " from nav");
    assert_eq!(deltas[2].2, "arguments");
    assert_eq!(deltas[2].3, r#"{"path":"fixture.txt"}"#);
    assert_eq!(deltas[0].0, deltas[1].0);
    assert_eq!(deltas[0].1, deltas[1].1);

    let completed = envelopes
        .iter()
        .find_map(|envelope| match &envelope.event {
            nav_protocol::BackendEvent::PartCompleted { part_id, .. } => Some(part_id),
            _ => None,
        })
        .expect("fixture should include part.completed");
    assert_eq!(
        completed,
        &PartId::try_new("prt_0000018bcfe56800_0000000000000001").unwrap()
    );
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

    let failed = envelopes
        .iter()
        .find(|e| e.event_type() == "tool.call_failed")
        .unwrap();
    match &failed.event {
        nav_protocol::BackendEvent::ToolCallFailed {
            name,
            error_message,
            ..
        } => {
            assert_eq!(name.as_deref(), Some("read"));
            assert!(!error_message.is_empty());
        }
        _ => panic!("expected tool.call_failed"),
    }
}

#[test]
fn tool_approval_requested_fixture_is_generic_hook_confirmation() {
    let fixture = "event-streams/tool-approval-requested.sse";
    let fixture_body = fixture_text(fixture);
    let events = parse_sse(&fixture_body);

    let approval = events
        .iter()
        .find(|event| event.name == "tool.approval_requested")
        .expect("fixture should include approval request");

    assert_eq!(approval.data["tool_name"].as_str(), Some("write_file"));
    assert_eq!(
        approval.data["reason"].as_str(),
        Some("writes outside the current task focus")
    );
    assert_eq!(
        approval.data["arguments_summary"].as_str(),
        Some(r#"{"path":"notes.md","content":"hello"}"#)
    );
    assert_eq!(approval.data["risk_class"].as_str(), Some("mutate"));
    assert_ne!(approval.data["tool_name"].as_str(), Some("bash"));

    let envelopes = event_envelopes(events);
    let encoded = sse::encode_events(&envelopes)
        .unwrap_or_else(|error| panic!("{fixture} should encode: {error}"));
    assert_eq!(encoded, fixture_body_with_final_separator(&fixture_body));

    let approval = envelopes
        .iter()
        .find(|event| event.event_type() == "tool.approval_requested")
        .expect("envelope should include approval request");
    match &approval.event {
        nav_protocol::BackendEvent::ToolApprovalRequested {
            tool_name,
            reason,
            arguments_summary,
            risk_class,
            ..
        } => {
            assert_eq!(tool_name, "write_file");
            assert_eq!(reason, "writes outside the current task focus");
            assert_eq!(
                arguments_summary,
                r#"{"path":"notes.md","content":"hello"}"#
            );
            assert_eq!(risk_class.as_deref(), Some("mutate"));
        }
        other => panic!("expected ToolApprovalRequested, got {other:?}"),
    }
}
