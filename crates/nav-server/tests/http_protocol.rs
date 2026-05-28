use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelInput, ModelRef, ModelSettings, ProviderCompat,
    ProviderConfig,
};
use nav_harness::sessions::PendingConfirmation;
use nav_harness::tools::{ToolRegistry, bash};
use nav_protocol::rpc::{SessionSource, ToolsPreset};
use nav_protocol::{BackendEvent, EventEnvelope};
use nav_server::http::{
    HttpRequest, HttpServer, HttpServerConfig, ProtocolEventSubscription, RunStatus,
};
use nav_types::{ApprovalId, RunId, SessionId, ToolCallId};
use serde_json::{Value, json};

mod support;

use support::{
    FakeProviderServer, HangingProviderServer, SequencedProviderServer,
    delayed_chat_completions_provider, provider_sse_chunk, successful_provider_chunks,
    successful_provider_with_text, unused_local_base_url, wait_for_run_status,
};

const LIVE_EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const RPC_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

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
fn session_create_stores_tools_preset_on_session_metadata() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());

    let create_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(102),
            "method": "session.create",
            "params": {
                "toolsPreset": "readonly"
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
    assert_eq!(metadata.tools_preset(), ToolsPreset::Readonly);
}

#[test]
fn session_create_defaults_tools_preset_to_coding() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());

    let create_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(103),
            "method": "session.create"
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
    assert_eq!(metadata.tools_preset(), ToolsPreset::Coding);
}

#[test]
fn session_create_rejects_invalid_tools_preset() {
    let mut server = HttpServer::with_model_settings(HttpServerConfig::default(), model_settings());

    let create_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(104),
            "method": "session.create",
            "params": {
                "toolsPreset": "unknown"
            }
        })
        .to_string(),
    ));

    assert_eq!(create_response.status(), 200);
    let create_body: Value = serde_json::from_str(create_response.body()).unwrap();
    assert_eq!(create_body["error"]["code"], -32602);
    assert!(
        create_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("invalid params")
    );
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
fn session_send_message_replays_previous_user_and_assistant_turns_to_provider() {
    let provider = SequencedProviderServer::start(vec![
        successful_provider_chunks("assistant remembered one"),
        successful_provider_chunks("assistant remembered two"),
    ]);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session(&mut server);

    let first_run_id = send_message_text(&mut server, &session_id, "first user turn");
    wait_for_run_status(&server, &first_run_id, RunStatus::Completed);
    let second_run_id = send_message_text(&mut server, &session_id, "second user turn");
    wait_for_run_status(&server, &second_run_id, RunStatus::Completed);

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].body["messages"],
        json!([
            { "role": "user", "content": "first user turn" },
            { "role": "assistant", "content": "assistant remembered one" },
            { "role": "user", "content": "second user turn" },
        ])
    );
}

#[test]
fn session_send_message_returns_structured_tool_error_for_unknown_tool() {
    let provider = SequencedProviderServer::start(vec![
        vec![
            provider_sse_chunk(
                r#"{"id":"provider-run","model":"vendor/model-large","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_missing_1","type":"function","function":{"name":"missing","arguments":"{}"}}]},"finish_reason":null}]}"#,
            ),
            provider_sse_chunk(
                r#"{"id":"provider-run","model":"vendor/model-large","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            ),
            "data: [DONE]\n\n".to_string(),
        ],
        successful_provider_chunks("unknown handled"),
    ]);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session(&mut server);

    let run_id = send_message(&mut server, &session_id);

    wait_for_run_status(&server, &run_id, RunStatus::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let tool_result = &requests[1].body["messages"][2];
    assert_eq!(tool_result["role"], "tool");
    assert_eq!(tool_result["tool_call_id"], "call_missing_1");
    let tool_error: Value = serde_json::from_str(tool_result["content"].as_str().unwrap())
        .expect("tool result should be structured JSON");
    assert_eq!(tool_error["ok"], false);
    assert_eq!(tool_error["error"]["message"], "unknown tool `missing`");

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

    let tool_call = events
        .iter()
        .find(|event| event.name == "tool.call_completed")
        .expect("tool call should be exposed before the loop continues");
    assert_eq!(tool_call.data["name"], "missing");
    assert_eq!(tool_call.data["arguments"], "{}");
}

#[test]
fn session_send_message_executes_read_tool_and_reenters_model_loop() {
    let workspace = TestWorkspace::new("read_tool_loop");
    workspace.write("fixture.txt", "alpha\nbeta\n");
    let provider = SequencedProviderServer::start(vec![
        vec![
            provider_sse_chunk(
                r#"{"id":"provider-run-1","model":"vendor/model-large","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_read_1","type":"function","function":{"name":"read","arguments":"{\"path\":\"fixture.txt\"}"}}]},"finish_reason":null}]}"#,
            ),
            provider_sse_chunk(
                r#"{"id":"provider-run-1","model":"vendor/model-large","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            ),
            "data: [DONE]\n\n".to_string(),
        ],
        successful_provider_chunks("read complete"),
    ]);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session_with_cwd(&mut server, workspace.root());

    let run_id = send_message_text(&mut server, &session_id, "read the fixture");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_names_from_request(&requests[0].body),
        vec!["bash", "edit", "read", "write"]
    );
    assert_eq!(
        requests[1].body["messages"],
        json!([
            { "role": "user", "content": "read the fixture" },
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_read_1",
                    "type": "function",
                    "function": {
                        "name": "read",
                        "arguments": "{\"path\":\"fixture.txt\"}"
                    }
                }]
            },
            {
                "role": "tool",
                "content": "1: alpha\n2: beta",
                "tool_call_id": "call_read_1"
            }
        ])
    );

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
            "tool.call_started",
            "tool.call_delta",
            "tool.call_completed",
            "message.completed",
            "model.text_delta",
            "message.completed",
            "run.completed",
        ]
    );
}

#[test]
fn session_send_message_executes_write_tool_and_publishes_file_changed() {
    let workspace = TestWorkspace::new("write_tool_loop");
    let tool_arguments = json!({
        "path": "notes/agent.md",
        "content": "hello from write\n",
    })
    .to_string();
    let provider = SequencedProviderServer::start(vec![
        write_tool_call_chunks("call_write_1", &tool_arguments),
        successful_provider_chunks("write complete"),
    ]);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session_with_cwd(&mut server, workspace.root());

    let run_id = send_message_text(&mut server, &session_id, "write the note");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);

    assert_eq!(
        fs::read_to_string(workspace.root().join("notes/agent.md"))
            .expect("write tool should create the requested file"),
        "hello from write\n"
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].body["messages"],
        json!([
            { "role": "user", "content": "write the note" },
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_write_1",
                    "type": "function",
                    "function": {
                        "name": "write",
                        "arguments": tool_arguments
                    }
                }]
            },
            {
                "role": "tool",
                "content": "wrote notes/agent.md",
                "tool_call_id": "call_write_1"
            }
        ])
    );

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
            "tool.call_started",
            "tool.call_delta",
            "tool.call_completed",
            "message.completed",
            "file.changed",
            "model.text_delta",
            "message.completed",
            "run.completed",
        ]
    );
    let file_changed = events
        .iter()
        .find(|event| event.name == "file.changed")
        .expect("write should publish file.changed");
    assert_eq!(file_changed.data["path"], "notes/agent.md");
    assert_uuid_v7(
        file_changed.data["file_change_id"]
            .as_str()
            .expect("file.changed should include file_change_id"),
    );
}

#[test]
fn session_send_message_executes_edit_tool_and_publishes_file_changed() {
    let workspace = TestWorkspace::new("edit_tool_loop");
    workspace.write("agent.md", "hello\nold line\n");
    let tool_arguments = json!({
        "path": "agent.md",
        "old_text": "old line",
        "new_text": "new line",
    })
    .to_string();
    let provider = SequencedProviderServer::start(vec![
        edit_tool_call_chunks("call_edit_1", &tool_arguments),
        successful_provider_chunks("edit complete"),
    ]);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session_with_cwd(&mut server, workspace.root());

    let run_id = send_message_text(&mut server, &session_id, "edit the note");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);

    assert_eq!(
        fs::read_to_string(workspace.root().join("agent.md"))
            .expect("edit tool should update the requested file"),
        "hello\nnew line\n"
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].body["messages"],
        json!([
            { "role": "user", "content": "edit the note" },
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_edit_1",
                    "type": "function",
                    "function": {
                        "name": "edit",
                        "arguments": tool_arguments
                    }
                }]
            },
            {
                "role": "tool",
                "content": "edited agent.md",
                "tool_call_id": "call_edit_1"
            }
        ])
    );

    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );
    assert!(
        events
            .iter()
            .any(|event| { event.name == "file.changed" && event.data["path"] == "agent.md" })
    );
}

#[test]
fn session_send_message_approves_guarded_bash_tool_and_resumes_run() {
    let mut fixture = GuardedBashFixture::new("call_bash_approve", "bash handled");
    let (run_id, approval_id) = fixture.wait_for_approval_request();

    assert_eq!(fixture.provider.request_count(), 1);
    assert_eq!(fixture.execution_count(), 0);
    approve_tool_request(&mut fixture.server, &approval_id);

    wait_for_run_status(&fixture.server, &run_id, RunStatus::Completed);
    assert_eq!(fixture.execution_count(), 1);
    assert_not_pending_confirmation(approve_tool_request_body(&mut fixture.server, &approval_id));
    assert_eq!(fixture.execution_count(), 1);
    let requests = fixture.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].body["messages"][2]["role"], "tool");
    assert_eq!(
        requests[1].body["messages"][2]["tool_call_id"],
        "call_bash_approve"
    );
    assert_eq!(requests[1].body["messages"][2]["content"], "bash output");
}

#[test]
fn approved_bash_tool_streams_output_deltas_before_completed_output() {
    let workspace = TestWorkspace::new("bash_streaming");
    let command = "printf 'first\\n'; sleep 0.15; printf 'second\\n'";
    let provider = SequencedProviderServer::start(vec![
        bash_tool_call_chunks("call_bash_stream", command),
        successful_provider_chunks("bash streaming handled"),
    ]);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    )
    .with_tool_registry(bash_tool_registry());
    let session_id =
        SessionId::try_new(create_session_with_cwd(&mut server, workspace.root())).unwrap();
    let subscription = server
        .subscribe_session_events(&session_id, None)
        .expect("session event subscription should open");

    let run_id = send_message_text(&mut server, session_id.as_str(), "run streaming bash");
    let pending_events = receive_live_events(&subscription, 6);
    assert_eq!(
        envelope_event_names(&pending_events),
        vec![
            "run.started",
            "tool.call_started",
            "tool.call_delta",
            "tool.call_completed",
            "message.completed",
            "tool.approval_requested",
        ]
    );
    let approval_id = approval_id_from_event(&pending_events[5]);

    let approved_at = Instant::now();
    approve_tool_request(&mut server, &approval_id);

    let first_delta = subscription
        .recv_timeout(LIVE_EVENT_TIMEOUT)
        .expect("first bash output delta should stream before completion");
    assert!(
        approved_at.elapsed() < Duration::from_millis(500),
        "first output delta should not wait for command completion"
    );
    assert_eq!(first_delta.event_type(), "tool.output_delta");
    assert_tool_output_delta(&first_delta, "stdout", "first\n");
    assert_eq!(
        server.run_status(&RunId::try_new(&run_id).unwrap()),
        Some(RunStatus::Running)
    );

    let remaining_events = receive_live_events(&subscription, 5);
    assert_eq!(
        envelope_event_names(&remaining_events),
        vec![
            "tool.output_delta",
            "tool.call_completed",
            "model.text_delta",
            "message.completed",
            "run.completed",
        ]
    );
    assert_tool_output_delta(&remaining_events[0], "stdout", "second\n");

    let completed = serde_json::to_value(&remaining_events[1]).unwrap();
    assert_eq!(completed["output"], "first\nsecond\n");
    assert_eq!(completed["output_lossy"], false);

    wait_for_run_status(&server, &run_id, RunStatus::Completed);
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].body["messages"][2]["content"],
        "first\nsecond\n"
    );
}

#[test]
fn session_send_message_rejects_guarded_bash_tool_without_execution() {
    let mut fixture = GuardedBashFixture::new("call_bash_reject", "bash rejected");
    let (run_id, approval_id) = fixture.wait_for_approval_request();

    reject_tool_request(&mut fixture.server, &approval_id, "not this command");

    wait_for_run_status(&fixture.server, &run_id, RunStatus::Completed);
    assert_eq!(fixture.execution_count(), 0);
    let requests = fixture.provider.requests();
    assert_eq!(requests.len(), 2);
    let tool_result = &requests[1].body["messages"][2];
    assert_eq!(tool_result["role"], "tool");
    assert_eq!(tool_result["tool_call_id"], "call_bash_reject");
    let rejection: Value = serde_json::from_str(tool_result["content"].as_str().unwrap())
        .expect("rejection should be structured JSON");
    assert_eq!(rejection["ok"], false);
    assert_eq!(rejection["error"]["code"], "tool_rejected");
    assert_eq!(rejection["error"]["reason"], "not this command");
}

#[test]
fn session_send_message_cancel_wakes_guarded_bash_confirmation_without_execution() {
    let mut fixture = GuardedBashFixture::new("call_bash_cancel", "should not be requested");
    let (run_id, approval_id) = fixture.wait_for_approval_request();

    cancel_run(&mut fixture.server, &run_id);

    wait_for_run_status(&fixture.server, &run_id, RunStatus::Cancelled);
    assert_eq!(fixture.execution_count(), 0);
    assert_eq!(fixture.provider.request_count(), 1);
    assert_not_pending_confirmation(approve_tool_request_body(&mut fixture.server, &approval_id));
    assert_eq!(fixture.execution_count(), 0);
}

#[test]
fn session_send_message_returns_structured_read_error_for_path_escape() {
    let workspace = TestWorkspace::new("read_tool_escape");
    let provider = SequencedProviderServer::start(vec![
        vec![
            provider_sse_chunk(
                r#"{"id":"provider-run-1","model":"vendor/model-large","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_read_escape","type":"function","function":{"name":"read","arguments":"{\"path\":\"../secret.txt\"}"}}]},"finish_reason":null}]}"#,
            ),
            provider_sse_chunk(
                r#"{"id":"provider-run-1","model":"vendor/model-large","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            ),
            "data: [DONE]\n\n".to_string(),
        ],
        successful_provider_chunks("escape handled"),
    ]);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session_with_cwd(&mut server, workspace.root());

    let run_id = send_message_text(&mut server, &session_id, "read outside the workspace");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    let tool_result = &requests[1].body["messages"][2];
    assert_eq!(tool_result["role"], "tool");
    assert_eq!(tool_result["tool_call_id"], "call_read_escape");
    let tool_error: Value = serde_json::from_str(tool_result["content"].as_str().unwrap())
        .expect("tool result should be structured JSON");
    assert_eq!(tool_error["ok"], false);
    assert!(
        tool_error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("escapes workspace")
    );
}

#[test]
fn session_send_message_keeps_interleaved_session_turns_isolated() {
    let provider = SequencedProviderServer::start(vec![
        successful_provider_chunks("assistant reply for session one"),
        successful_provider_chunks("assistant reply for session two"),
        successful_provider_chunks("second reply for session one"),
    ]);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig::default(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_one = create_session(&mut server);
    let session_two = create_session(&mut server);

    let first_one = send_message_text(&mut server, &session_one, "session one first user turn");
    wait_for_run_status(&server, &first_one, RunStatus::Completed);
    let first_two = send_message_text(&mut server, &session_two, "session two first user turn");
    wait_for_run_status(&server, &first_two, RunStatus::Completed);
    let second_one = send_message_text(&mut server, &session_one, "session one second user turn");
    wait_for_run_status(&server, &second_one, RunStatus::Completed);

    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].body["messages"],
        json!([
            { "role": "user", "content": "session one first user turn" },
            { "role": "assistant", "content": "assistant reply for session one" },
            { "role": "user", "content": "session one second user turn" },
        ])
    );
    assert!(
        !requests[2]
            .body
            .to_string()
            .contains("session two first user turn")
    );
    assert!(
        !requests[2]
            .body
            .to_string()
            .contains("assistant reply for session two")
    );
}

#[test]
fn session_send_message_returns_before_provider_stream_finishes() {
    let provider = HangingProviderServer::start();
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

    let session_id_for_send = session_id.clone();
    let (send_tx, send_rx) = mpsc::channel();
    let send_handle = thread::spawn(move || {
        let send = send_message_ids(&mut server, session_id_for_send.as_str(), request_id(8));
        let _ = send_tx.send((server, send));
    });
    let (mut server, send) = match send_rx.recv_timeout(RPC_RESPONSE_TIMEOUT) {
        Ok(result) => result,
        Err(error) => {
            provider.stop();
            panic!("session.sendMessage should return before provider stream finishes: {error}");
        }
    };
    send_handle
        .join()
        .expect("session.sendMessage thread should finish");

    let request = provider.wait_for_request();
    assert_eq!(request.path, "/v1/chat/completions");
    assert_uuid_v7(&send.run_id);
    assert_uuid_v7(&send.message_id);
    assert_eq!(
        server.run_status(&RunId::try_new(&send.run_id).unwrap()),
        Some(RunStatus::Running)
    );

    let live_events = receive_live_events(&subscription, 1);
    assert_eq!(envelope_event_names(&live_events), vec!["run.started"]);
    assert_eq!(live_events[0].session_id, session_id);
    assert!(matches!(
        subscription.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let cancel_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(9),
            "method": "run.cancel",
            "params": { "runId": send.run_id.as_str() }
        })
        .to_string(),
    ));
    let cancel_body: Value = serde_json::from_str(cancel_response.body()).unwrap();
    assert_eq!(cancel_body["result"]["runId"], send.run_id);
    wait_for_run_status(&server, &send.run_id, RunStatus::Cancelled);
    provider.stop();

    let cancel_events = receive_live_events(&subscription, 1);
    assert_eq!(envelope_event_names(&cancel_events), vec!["run.cancelled"]);
    assert_eq!(cancel_events[0].session_id, session_id);
}

#[test]
fn session_send_message_streams_delayed_provider_chunks_live_and_replays_midstream() {
    let mut provider = delayed_chat_completions_provider();
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
    let request = provider.wait_for_request();
    assert_eq!(request.path, "/v1/chat/completions");

    let live_before_completion = receive_live_events(&subscription, 2);
    assert_eq!(
        envelope_event_names(&live_before_completion),
        vec!["run.started", "model.text_delta"]
    );
    assert_model_text_delta(&live_before_completion[1], "hello ");
    assert!(matches!(
        subscription.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(
        server.run_status(&RunId::try_new(&run_id).unwrap()),
        Some(RunStatus::Running)
    );

    let replay_after_run_started = server
        .subscribe_session_events_http(
            session_id.as_str(),
            Some(live_before_completion[0].event_id.as_str()),
        )
        .expect("midstream replay subscription should open");
    assert_eq!(
        envelope_event_names(replay_after_run_started.replay()),
        vec!["model.text_delta"]
    );
    assert_model_text_delta(&replay_after_run_started.replay()[0], "hello ");

    provider.release_completion();

    let live_after_release = receive_live_events(&subscription, 3);
    assert_eq!(
        envelope_event_names(&live_after_release),
        vec!["model.text_delta", "message.completed", "run.completed"]
    );
    assert_model_text_delta(&live_after_release[0], "Season");

    let replay_after_release = receive_live_events(&replay_after_run_started, 3);
    assert_eq!(
        envelope_event_names(&replay_after_release),
        vec!["model.text_delta", "message.completed", "run.completed"]
    );
    assert_model_text_delta(&replay_after_release[0], "Season");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);
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
    let pending_approval_id = approval_id(70);
    server
        .register_pending_confirmation(pending_confirmation(&run_id, pending_approval_id.clone()))
        .expect("pending confirmation should register");

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

    let approve_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(8),
            "method": "tool.approve",
            "params": { "approval_id": pending_approval_id }
        })
        .to_string(),
    ));
    let approve_body: Value = serde_json::from_str(approve_response.body()).unwrap();
    assert_eq!(approve_body["error"]["code"], -32006);
    assert!(
        approve_body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not pending")
    );
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

fn assert_model_text_delta(event: &EventEnvelope, expected: &str) {
    match &event.event {
        BackendEvent::ModelTextDelta { delta, .. } => assert_eq!(delta, expected),
        event => panic!("event = {event:?}, want model.text_delta"),
    }
}

fn assert_tool_output_delta(event: &EventEnvelope, expected_stream: &str, expected_chunk: &str) {
    let payload = serde_json::to_value(event).expect("event should serialize");
    assert_eq!(payload["stream"], expected_stream);
    assert_eq!(payload["chunk"], expected_chunk);
}

fn approval_id_from_event(event: &EventEnvelope) -> String {
    match &event.event {
        BackendEvent::ToolApprovalRequested { approval_id, .. } => approval_id.to_string(),
        event => panic!("event = {event:?}, want tool.approval_requested"),
    }
}

fn tool_names_from_request(body: &Value) -> Vec<&str> {
    body["tools"]
        .as_array()
        .expect("request tools should be an array")
        .iter()
        .map(|tool| {
            tool["function"]["name"]
                .as_str()
                .expect("tool function name should be a string")
        })
        .collect()
}

struct GuardedBashFixture {
    server: HttpServer,
    provider: SequencedProviderServer,
    _workspace: TestWorkspace,
    execution_marker: PathBuf,
    session_id: SessionId,
    subscription: ProtocolEventSubscription,
}

impl GuardedBashFixture {
    fn new(provider_call_id: &str, final_response: &str) -> Self {
        let workspace = TestWorkspace::new(provider_call_id);
        let execution_marker = workspace.root.join("bash-executions.txt");
        let command = format!(
            "printf 'bash output'; printf 'run\\n' >> {}",
            shell_quote_path(&execution_marker)
        );
        let provider = SequencedProviderServer::start(vec![
            bash_tool_call_chunks(provider_call_id, &command),
            successful_provider_chunks(final_response),
        ]);
        let mut server = HttpServer::with_model_settings(
            HttpServerConfig::default(),
            model_settings_with_base_url(provider.base_url()),
        )
        .with_tool_registry(bash_tool_registry());
        let session_id =
            SessionId::try_new(create_session_with_cwd(&mut server, workspace.root())).unwrap();
        let subscription = server
            .subscribe_session_events(&session_id, None)
            .expect("session event subscription should open");

        Self {
            server,
            provider,
            _workspace: workspace,
            execution_marker,
            session_id,
            subscription,
        }
    }

    fn wait_for_approval_request(&mut self) -> (String, String) {
        let run_id = send_message_text(&mut self.server, self.session_id.as_str(), "run bash");
        let pending_events = receive_live_events(&self.subscription, 6);
        assert_eq!(
            envelope_event_names(&pending_events),
            vec![
                "run.started",
                "tool.call_started",
                "tool.call_delta",
                "tool.call_completed",
                "message.completed",
                "tool.approval_requested",
            ]
        );
        let approval_id = approval_id_from_event(&pending_events[5]);
        (run_id, approval_id)
    }

    fn execution_count(&self) -> usize {
        fs::read_to_string(&self.execution_marker)
            .map(|content| content.lines().count())
            .unwrap_or(0)
    }
}

fn bash_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::default();
    bash::register(&mut registry).expect("bash test tool should register");
    registry
}

fn approve_tool_request(server: &mut HttpServer, approval_id: &str) {
    let body = approve_tool_request_body(server, approval_id);
    assert_eq!(body["result"]["outcome"], "approved");
}

fn approve_tool_request_body(server: &mut HttpServer, approval_id: &str) -> Value {
    let response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(70),
            "method": "tool.approve",
            "params": { "approval_id": approval_id }
        })
        .to_string(),
    ));
    serde_json::from_str(response.body()).unwrap()
}

fn reject_tool_request(server: &mut HttpServer, approval_id: &str, reason: &str) {
    let response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(71),
            "method": "tool.reject",
            "params": {
                "approval_id": approval_id,
                "reason": reason
            }
        })
        .to_string(),
    ));
    let body: Value = serde_json::from_str(response.body()).unwrap();
    assert_eq!(body["result"]["outcome"], "rejected");
}

fn cancel_run(server: &mut HttpServer, run_id: &str) {
    let response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(72),
            "method": "run.cancel",
            "params": { "runId": run_id }
        })
        .to_string(),
    ));
    let body: Value = serde_json::from_str(response.body()).unwrap();
    assert_eq!(body["result"]["runId"], run_id);
}

fn assert_not_pending_confirmation(body: Value) {
    assert_eq!(body["error"]["code"], -32006);
    assert!(
        body["error"]["message"]
            .as_str()
            .expect("error message should be a string")
            .contains("not pending")
    );
}

fn bash_tool_call_chunks(provider_call_id: &str, command: &str) -> Vec<String> {
    let tool_arguments = json!({ "command": command }).to_string();
    tool_call_chunks("bash", provider_call_id, &tool_arguments)
}

fn write_tool_call_chunks(provider_call_id: &str, tool_arguments: &str) -> Vec<String> {
    tool_call_chunks("write", provider_call_id, tool_arguments)
}

fn edit_tool_call_chunks(provider_call_id: &str, tool_arguments: &str) -> Vec<String> {
    tool_call_chunks("edit", provider_call_id, tool_arguments)
}

fn tool_call_chunks(tool_name: &str, provider_call_id: &str, tool_arguments: &str) -> Vec<String> {
    let chunk = json!({
        "id": "provider-run-1",
        "model": "vendor/model-large",
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": provider_call_id,
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": tool_arguments
                    }
                }]
            },
            "finish_reason": null
        }]
    });
    vec![
        provider_sse_chunk(&chunk.to_string()),
        provider_sse_chunk(
            r#"{"id":"provider-run-1","model":"vendor/model-large","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ),
        "data: [DONE]\n\n".to_string(),
    ]
}

fn create_session(server: &mut HttpServer) -> String {
    create_session_with_cwd(server, Path::new("/tmp/nav-workspace"))
}

fn shell_quote_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn create_session_with_cwd(server: &mut HttpServer, cwd: &Path) -> String {
    let create_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(99),
            "method": "session.create",
            "params": { "cwd": cwd.display().to_string() }
        })
        .to_string(),
    ));
    let create_body: Value = serde_json::from_str(create_response.body()).unwrap();
    create_body["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_string()
}

fn pending_confirmation(run_id: &str, approval_id: ApprovalId) -> PendingConfirmation {
    PendingConfirmation {
        approval_id,
        run_id: RunId::try_new(run_id).unwrap(),
        tool_call_id: ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap(),
        tool_name: "write_file".to_string(),
        reason: "writes outside the current task focus".to_string(),
        arguments_summary: r#"{"path":"notes.md","content":"hello"}"#.to_string(),
        risk_class: Some("mutate".to_string()),
    }
}

struct TestWorkspace {
    root: PathBuf,
}

impl TestWorkspace {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nav-server-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("workspace should be created");
        Self {
            root: fs::canonicalize(root).expect("workspace should canonicalize"),
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn write(&self, relative_path: &str, content: &str) {
        fs::write(self.root.join(relative_path), content).expect("file should be written");
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug)]
struct SendMessageIds {
    run_id: String,
    message_id: String,
}

fn send_message(server: &mut HttpServer, session_id: &str) -> String {
    send_message_text(server, session_id, "hello")
}

fn send_message_text(server: &mut HttpServer, session_id: &str, text: &str) -> String {
    send_message_ids_with_text(server, session_id, request_id(98), text).run_id
}

fn send_message_ids(
    server: &mut HttpServer,
    session_id: &str,
    rpc_request_id: String,
) -> SendMessageIds {
    send_message_ids_with_text(server, session_id, rpc_request_id, "hello")
}

fn send_message_ids_with_text(
    server: &mut HttpServer,
    session_id: &str,
    rpc_request_id: String,
    text: &str,
) -> SendMessageIds {
    let send_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": rpc_request_id,
            "method": "session.sendMessage",
            "params": { "sessionId": session_id, "text": text }
        })
        .to_string(),
    ));
    let send_body: Value = serde_json::from_str(send_response.body()).unwrap();
    SendMessageIds {
        run_id: send_body["result"]["runId"].as_str().unwrap().to_string(),
        message_id: send_body["result"]["messageId"]
            .as_str()
            .unwrap()
            .to_string(),
    }
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

fn approval_id(index: u64) -> ApprovalId {
    ApprovalId::try_new(format!("019f2f6f-f178-7a72-9f28-{index:012x}")).unwrap()
}

fn assert_uuid_v7(value: &str) {
    assert_eq!(value.len(), 36);
    assert_eq!(&value[14..15], "7");
    assert!(matches!(&value[19..20], "8" | "9" | "a" | "b"));
}
