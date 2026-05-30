use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use nav_harness::guardrails::{BashConfirmationHook, GuardrailRunner};
use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelInput, ModelRef, ModelSettings, ProviderCompat,
    ProviderConfig,
};
use nav_harness::sessions::{
    ModelTurn, Part, PendingConfirmation, RevertInfo, SessionStore, SqliteSessionStore,
};
use nav_harness::tools::{ToolRegistry, bash};
use nav_protocol::rpc::{SessionSource, ToolsPreset};
use nav_protocol::{BackendEvent, EventEnvelope};
use nav_server::http::{
    HttpRequest, HttpServer, HttpServerConfig, ProtocolEventSubscription, RunStatus,
};
use nav_types::{ApprovalId, MessageId, PartId, RunId, SessionId, ToolCallId};
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
fn session_search_returns_cjk_substring_hit_with_anchored_bookends() {
    let db = TestSessionDb::new("fts-search-route-cjk");
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut server = HttpServer::with_model_settings(config, model_settings());
    let session_id = SessionId::try_new(create_session(&mut server)).unwrap();
    let run_id = RunId::new_unchecked("019f2f6f-f178-7a72-9f28-000000000479");
    let tool_call_id = ToolCallId::new_unchecked("019f2f6f-f178-7a72-9f28-00000000047a");
    let seed_store = SessionStore::open(db.path()).expect("seed store should open");

    seed_store
        .start_run(&session_id, run_id.clone())
        .expect("run should start");
    for (index, text) in [
        "goal: inspect session search",
        "opening assistant context",
        "nearby setup detail",
        "我们在火星基地测试量子导航",
        "after the target",
        "resolution verified",
        "done shipping search",
    ]
    .into_iter()
    .enumerate()
    {
        let message_id =
            MessageId::new_unchecked(format!("019f2f6f-f178-7a72-9f28-0000000004{:02x}", index));
        let turn = if text.contains("火星基地") {
            ModelTurn::tool_result(tool_call_id.as_str(), text)
        } else if index % 2 == 0 {
            ModelTurn::user_text(text)
        } else {
            ModelTurn::assistant_text(text)
        };
        seed_store
            .append_turn(&run_id, message_id, turn)
            .expect("turn should append");
    }

    let search_response = server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(479),
            "method": "session.search",
            "params": {
                "query": "星基地",
                "limit": 1,
                "surroundingTurns": 1,
                "index": "trigram"
            }
        })
        .to_string(),
    ));

    assert_eq!(search_response.status(), 200);
    let body: Value = serde_json::from_str(search_response.body()).unwrap();
    let hit = &body["result"]["hits"][0];
    assert_eq!(hit["sessionId"], session_id.as_str());
    assert_eq!(
        hit["anchoredTurns"][3]["parts"][0]["partType"],
        "tool_result"
    );
    assert_eq!(hit["text"], "我们在火星基地测试量子导航");

    let context_texts = hit["anchoredTurns"]
        .as_array()
        .expect("anchored turns should be present")
        .iter()
        .flat_map(|turn| {
            turn["parts"]
                .as_array()
                .expect("turn parts should be present")
                .iter()
                .map(|part| part["text"].as_str().expect("text part should expose text"))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        context_texts,
        vec![
            "goal: inspect session search",
            "opening assistant context",
            "nearby setup detail",
            "我们在火星基地测试量子导航",
            "after the target",
            "resolution verified",
            "done shipping search",
        ]
    );
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
                "settingsJson": {
                    "modelRef": {
                        "provider": "compatible-gateway",
                        "model": "vendor/model-large"
                    }
                },
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
fn session_create_preserves_tools_preset_after_backend_restart() {
    let db = TestSessionDb::new("restart-tools-preset");
    let provider = successful_provider_with_text("readonly session survived restart");
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut first_server = HttpServer::with_model_settings(
        config.clone(),
        model_settings_with_base_url(provider.base_url()),
    );

    let create_response = first_server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(105),
            "method": "session.create",
            "params": {
                "settingsJson": {
                    "modelRef": {
                        "provider": "compatible-gateway",
                        "model": "vendor/model-large"
                    }
                },
                "toolsPreset": "readonly"
            }
        })
        .to_string(),
    ));
    let create_body: Value = serde_json::from_str(create_response.body()).unwrap();
    let session_id = create_body["result"]["sessionId"]
        .as_str()
        .expect("session.create should return a session id")
        .to_string();
    drop(first_server);

    let mut second_server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let run_id = send_message_text(&mut second_server, &session_id, "which tools can you use?");
    wait_for_run_status(&second_server, &run_id, RunStatus::Completed);

    let request = provider.request();
    assert_eq!(tool_names_from_request(&request.body), vec!["read"]);
    let session_id = SessionId::try_new(session_id).unwrap();
    let metadata = second_server
        .session_metadata(&session_id)
        .expect("session metadata should reload from SQLite");
    assert_eq!(metadata.tools_preset(), ToolsPreset::Readonly);
    assert_eq!(
        metadata.settings_json().unwrap()["modelRef"]["provider"],
        "compatible-gateway"
    );
    assert!(
        metadata
            .settings_json()
            .unwrap()
            .get("__navToolsPreset")
            .is_none()
    );
}

#[test]
fn session_create_ignores_reserved_tools_preset_inside_settings_json_after_backend_restart() {
    let db = TestSessionDb::new("restart-settings-reserved-tools-preset");
    let provider = successful_provider_with_text("settings cannot spoof tools");
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut first_server = HttpServer::with_model_settings(
        config.clone(),
        model_settings_with_base_url(provider.base_url()),
    );

    let create_response = first_server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(106),
            "method": "session.create",
            "params": {
                "settingsJson": {
                    "__navToolsPreset": "readonly",
                    "modelRef": {
                        "provider": "compatible-gateway",
                        "model": "vendor/model-large"
                    }
                }
            }
        })
        .to_string(),
    ));
    let create_body: Value = serde_json::from_str(create_response.body()).unwrap();
    let session_id = create_body["result"]["sessionId"]
        .as_str()
        .expect("session.create should return a session id")
        .to_string();
    drop(first_server);

    let mut second_server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let run_id = send_message_text(&mut second_server, &session_id, "which tools can you use?");
    wait_for_run_status(&second_server, &run_id, RunStatus::Completed);

    let request = provider.request();
    assert_eq!(tool_names_from_request(&request.body), coding_tool_names());
    let session_id = SessionId::try_new(session_id).unwrap();
    let metadata = second_server
        .session_metadata(&session_id)
        .expect("session metadata should reload from SQLite");
    assert_eq!(metadata.tools_preset(), ToolsPreset::Coding);
    assert!(
        metadata
            .settings_json()
            .unwrap()
            .get("__navToolsPreset")
            .is_none()
    );
}

#[test]
fn session_create_preserves_settings_json_object_with_settings_json_key_after_backend_restart() {
    let db = TestSessionDb::new("restart-settings-json-key");
    let provider = successful_provider_with_text("settings shape survived restart");
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut first_server = HttpServer::with_model_settings(
        config.clone(),
        model_settings_with_base_url(provider.base_url()),
    );

    let create_response = first_server.handle_request(HttpRequest::post(
        "/rpc",
        json!({
            "jsonrpc": "2.0",
            "id": request_id(107),
            "method": "session.create",
            "params": {
                "settingsJson": {
                    "settingsJson": "user-visible"
                }
            }
        })
        .to_string(),
    ));
    let create_body: Value = serde_json::from_str(create_response.body()).unwrap();
    let session_id = create_body["result"]["sessionId"]
        .as_str()
        .expect("session.create should return a session id")
        .to_string();
    drop(first_server);

    let mut second_server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let run_id = send_message_text(&mut second_server, &session_id, "which settings survived?");
    wait_for_run_status(&second_server, &run_id, RunStatus::Completed);

    let session_id = SessionId::try_new(session_id).unwrap();
    let metadata = second_server
        .session_metadata(&session_id)
        .expect("session metadata should reload from SQLite");
    assert_eq!(
        metadata.settings_json().unwrap(),
        &json!({ "settingsJson": "user-visible" })
    );
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
            "session.totals_updated",
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
        vec![
            "model.text_delta",
            "message.completed",
            "run.completed",
            "session.totals_updated"
        ]
    );
    assert_protocol_event_ids(&replayed_events, session_id);
}

#[test]
fn completed_run_journals_provider_response_and_decoded_parts() {
    let db = TestSessionDb::new("provider-response-journal");
    let provider = successful_provider_with_text("journaled reply");
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let session_id = create_session(&mut server);

    let run_id = send_message_text(&mut server, &session_id, "persist the envelope");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);
    drop(server);

    let store = SqliteSessionStore::open(db.path()).expect("store should reopen");
    let payloads = store
        .list_decoded_provider_payloads()
        .expect("provider payloads should list");
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].direction, "response");
    assert_eq!(payloads[0].decode_status, "decoded");

    let parts = store
        .list_parts_for_provider_payload(&payloads[0].id)
        .expect("decoded parts should list");
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].provider_payload_id, Some(payloads[0].id.clone()));
    assert_eq!(
        parts[0].provider_json_pointer.as_deref(),
        Some("/choices/0/message/content")
    );
    assert_eq!(parts[0].part, text_part("journaled reply"));
}

#[test]
fn session_send_message_clears_pending_revert_metadata() {
    let db = TestSessionDb::new("send-clears-revert");
    let provider = successful_provider_with_text("continued after undo point");
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let session_id = create_session(&mut server);
    let parsed_session_id = SessionId::try_new(session_id.clone()).unwrap();
    let revert = RevertInfo {
        message_id: MessageId::new_unchecked("019e7000-0000-7000-8000-000000000542"),
        part_id: Some(PartId::new_unchecked(
            "prt_0000018bcfe56800_0000000000000542",
        )),
        snapshot: Some("snapshot-before-continue".to_string()),
        diff: Some("diff --git a/file.txt b/file.txt\n+assistant change\n".to_string()),
    };
    {
        let store = SessionStore::open(db.path()).expect("store should open");
        store
            .update_session_revert(&parsed_session_id, &revert)
            .expect("revert metadata update should commit");
        assert!(
            store
                .get_session(&parsed_session_id)
                .expect("session should be readable")
                .revert_json
                .is_some()
        );
    }

    let run_id = send_message_text(&mut server, &session_id, "continue after undo point");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);
    drop(server);

    let store = SessionStore::open(db.path()).expect("store should reopen");
    let session = store
        .get_session(&parsed_session_id)
        .expect("session should be readable");

    assert_eq!(session.revert_json, None);
}

#[test]
fn completed_run_journals_outgoing_provider_request_body() {
    let db = TestSessionDb::new("provider-request-journal");
    let provider = successful_provider_with_text("request journaled");
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let session_id = create_session(&mut server);

    let run_id = send_message_text(&mut server, &session_id, "persist the request");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);
    let provider_request = provider.request();
    drop(server);

    let store = SqliteSessionStore::open(db.path()).expect("store should reopen");
    let run_id = RunId::try_new(run_id).expect("run id should parse");
    let payloads = store
        .list_provider_payloads_for_run(&run_id)
        .expect("provider payloads should list");
    let directions = payloads
        .iter()
        .map(|payload| payload.direction.as_str())
        .collect::<Vec<_>>();
    assert_eq!(directions, vec!["request", "response"]);

    let request_payload = &payloads[0];
    assert_eq!(request_payload.decode_status, "ignored");
    let journaled_request: Value =
        serde_json::from_slice(&artifact_bytes(&store, &request_payload.artifact_id))
            .expect("request artifact should be JSON");
    assert_eq!(journaled_request, provider_request.body);
    assert_eq!(journaled_request["stream"], true);
}

#[test]
fn completed_high_delta_stream_persists_one_assistant_part() {
    let db = TestSessionDb::new("high-delta-stream-batch");
    let deltas = (0..100)
        .map(|index| format!("chunk-{index:03};"))
        .collect::<Vec<_>>();
    let mut chunks = deltas
        .iter()
        .map(|delta| {
            provider_sse_chunk(
                &json!({
                    "id": "provider-run",
                    "model": "vendor/model-large",
                    "choices": [{
                        "index": 0,
                        "delta": { "content": delta },
                        "finish_reason": null
                    }]
                })
                .to_string(),
            )
        })
        .collect::<Vec<_>>();
    chunks.push(provider_sse_chunk(
        r#"{"id":"provider-run","model":"vendor/model-large","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
    ));
    chunks.push("data: [DONE]\n\n".to_string());
    let provider = FakeProviderServer::start(200, "text/event-stream", chunks);
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let session_id = create_session(&mut server);

    let run_id = send_message_text(&mut server, &session_id, "batch stream");

    assert_eq!(provider.request().path, "/v1/chat/completions");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);
    let events = parse_sse(
        server
            .handle_request(HttpRequest::get(format!("/sessions/{session_id}/events")))
            .body(),
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.name == "model.text_delta")
            .count(),
        deltas.len()
    );
    drop(server);

    let expected_text = deltas.concat();
    let store = SqliteSessionStore::open(db.path()).expect("store should reopen");
    let run_id = RunId::try_new(run_id).expect("run id should parse");
    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].1.len(), 1);
    assert_eq!(turns[1].1[0].part, text_part(expected_text.clone()));

    let payloads = store
        .list_provider_payloads_for_run(&run_id)
        .expect("provider payloads should list");
    assert_eq!(
        payloads
            .iter()
            .map(|payload| payload.direction.as_str())
            .collect::<Vec<_>>(),
        vec!["request", "response"]
    );
    let response_parts = store
        .list_parts_for_provider_payload(&payloads[1].id)
        .expect("response payload parts should list");
    assert_eq!(response_parts.len(), 1);
    assert_eq!(response_parts[0].part, text_part(expected_text));
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
            "session.totals_updated",
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
fn session_history_survives_backend_restart_for_next_run() {
    let db = TestSessionDb::new("restart-history");
    let provider = SequencedProviderServer::start(vec![
        successful_provider_chunks("assistant before restart"),
        successful_provider_chunks("assistant after restart"),
    ]);
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut first_server = HttpServer::with_model_settings(
        config.clone(),
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session(&mut first_server);

    let first_run_id = send_message_text(&mut first_server, &session_id, "first before restart");
    wait_for_run_status(&first_server, &first_run_id, RunStatus::Completed);
    drop(first_server);

    let mut second_server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let second_run_id = send_message_text(&mut second_server, &session_id, "second after restart");
    wait_for_run_status(&second_server, &second_run_id, RunStatus::Completed);

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].body["messages"],
        json!([
            { "role": "user", "content": "first before restart" },
            { "role": "assistant", "content": "assistant before restart" },
            { "role": "user", "content": "second after restart" },
        ])
    );
    drop(second_server);

    let store = SqliteSessionStore::open(db.path()).expect("store should reopen");
    let second_run_id = RunId::try_new(second_run_id).expect("second run id should parse");
    let payloads = store
        .list_provider_payloads_for_run(&second_run_id)
        .expect("provider payloads should list");
    assert_eq!(payloads[0].direction, "request");
    let journaled_request: Value =
        serde_json::from_slice(&artifact_bytes(&store, &payloads[0].artifact_id))
            .expect("request artifact should be JSON");
    assert_eq!(journaled_request, requests[1].body);
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
            "session.totals_updated",
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
            "session.totals_updated",
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
            "session.totals_updated",
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
fn session_send_message_records_snapshot_for_edit_write_sequence_and_revert_restores_workspace() {
    let db = TestSessionDb::new("edit-write-revert");
    let workspace = TestWorkspace::new("edit_write_revert");
    workspace.write("agent.md", "hello\nold line\n");
    let edit_arguments = json!({
        "path": "agent.md",
        "old_text": "old line",
        "new_text": "new line",
    })
    .to_string();
    let write_arguments = json!({
        "path": "created/agent.md",
        "content": "created by assistant\n",
    })
    .to_string();
    let tool_call_chunk = json!({
        "id": "provider-run-1",
        "model": "vendor/model-large",
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [
                    {
                        "index": 0,
                        "id": "call_edit_1",
                        "type": "function",
                        "function": {
                            "name": "edit",
                            "arguments": edit_arguments,
                        },
                    },
                    {
                        "index": 1,
                        "id": "call_write_1",
                        "type": "function",
                        "function": {
                            "name": "write",
                            "arguments": write_arguments,
                        },
                    },
                ],
            },
            "finish_reason": null,
        }],
    });
    let provider = SequencedProviderServer::start(vec![
        vec![
            provider_sse_chunk(&tool_call_chunk.to_string()),
            provider_sse_chunk(
                r#"{"id":"provider-run-1","model":"vendor/model-large","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            ),
            "data: [DONE]\n\n".to_string(),
        ],
        successful_provider_chunks("changes complete"),
    ]);
    let mut server = HttpServer::with_model_settings(
        HttpServerConfig {
            session_db_path: Some(db.path().to_path_buf()),
            ..HttpServerConfig::default()
        },
        model_settings_with_base_url(provider.base_url()),
    );
    let session_id = create_session_with_cwd(&mut server, workspace.root());

    let run_id = send_message_text(&mut server, &session_id, "edit and write");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);

    assert_eq!(
        fs::read_to_string(workspace.root().join("agent.md"))
            .expect("edit tool should update the existing file"),
        "hello\nnew line\n"
    );
    assert_eq!(
        fs::read_to_string(workspace.root().join("created/agent.md"))
            .expect("write tool should create the requested file"),
        "created by assistant\n"
    );

    let store = SessionStore::open(db.path()).expect("session store should reopen");
    let parsed_session_id = SessionId::try_new(session_id).unwrap();
    let revert_json = store
        .get_session(&parsed_session_id)
        .unwrap()
        .revert_json
        .expect("revert snapshot metadata should be recorded");
    assert!(
        revert_json.contains("art_"),
        "revert metadata should reference a snapshot artifact: {revert_json}"
    );

    store.revert_to(&parsed_session_id).unwrap();

    assert_eq!(
        fs::read_to_string(workspace.root().join("agent.md"))
            .expect("reverted file should be readable"),
        "hello\nold line\n"
    );
    assert!(
        !workspace.root().join("created/agent.md").exists(),
        "revert should remove files that did not exist before the assistant turn"
    );
    assert!(
        !workspace.root().join("created").exists(),
        "revert should remove parent directories created only for the assistant write"
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
fn default_bash_tool_streams_output_deltas_without_confirmation() {
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
    let initial_events = receive_live_events(&subscription, 5);
    assert_eq!(
        envelope_event_names(&initial_events),
        vec![
            "run.started",
            "tool.call_started",
            "tool.call_delta",
            "tool.call_completed",
            "message.completed",
        ]
    );

    let first_delta_wait_started = Instant::now();
    let first_delta = subscription
        .recv_timeout(LIVE_EVENT_TIMEOUT)
        .expect("first bash output delta should stream before completion");
    assert!(
        first_delta_wait_started.elapsed() < Duration::from_millis(500),
        "first output delta should not wait for command completion"
    );
    assert_eq!(first_delta.event_type(), "tool.output_delta");
    assert_tool_output_delta(&first_delta, "stdout", "first\n");
    assert_eq!(
        server.run_status(&RunId::try_new(&run_id).unwrap()),
        Some(RunStatus::Running)
    );

    let remaining_events = receive_live_events(&subscription, 6);
    assert_eq!(
        envelope_event_names(&remaining_events),
        vec![
            "tool.output_delta",
            "tool.call_completed",
            "model.text_delta",
            "message.completed",
            "run.completed",
            "session.totals_updated",
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

    let live_after_release = receive_live_events(&subscription, 4);
    assert_eq!(
        envelope_event_names(&live_after_release),
        vec![
            "model.text_delta",
            "message.completed",
            "run.completed",
            "session.totals_updated"
        ]
    );
    assert_model_text_delta(&live_after_release[0], "Season");

    let replay_after_release = receive_live_events(&replay_after_run_started, 4);
    assert_eq!(
        envelope_event_names(&replay_after_release),
        vec![
            "model.text_delta",
            "message.completed",
            "run.completed",
            "session.totals_updated"
        ]
    );
    assert_model_text_delta(&replay_after_release[0], "Season");
    wait_for_run_status(&server, &run_id, RunStatus::Completed);
}

#[test]
fn streaming_delta_is_not_persisted_until_turn_boundary() {
    let db = TestSessionDb::new("streaming-delta-turn-boundary");
    let mut provider = delayed_chat_completions_provider();
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let session_id = SessionId::try_new(create_session(&mut server)).unwrap();
    let subscription = server
        .subscribe_session_events(&session_id, None)
        .expect("session event subscription should open");

    let run_id = send_message_text(&mut server, session_id.as_str(), "persist at boundary");
    assert_eq!(provider.wait_for_request().path, "/v1/chat/completions");

    let live_before_completion = receive_live_events(&subscription, 2);
    assert_eq!(
        envelope_event_names(&live_before_completion),
        vec!["run.started", "model.text_delta"]
    );
    assert_model_text_delta(&live_before_completion[1], "hello ");

    let store = SqliteSessionStore::open(db.path()).expect("store should open midstream");
    let run_id = RunId::try_new(run_id).expect("run id should parse");
    let midstream_turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should read midstream");
    assert_eq!(midstream_turns.len(), 1);
    assert_eq!(
        midstream_turns[0].1[0].part,
        text_part("persist at boundary")
    );
    let midstream_payloads = store
        .list_provider_payloads_for_run(&run_id)
        .expect("payloads should read midstream");
    assert_eq!(
        midstream_payloads
            .iter()
            .map(|payload| payload.direction.as_str())
            .collect::<Vec<_>>(),
        vec!["request"]
    );
    drop(store);

    provider.release_completion();
    let live_after_release = receive_live_events(&subscription, 4);
    assert_eq!(
        envelope_event_names(&live_after_release),
        vec![
            "model.text_delta",
            "message.completed",
            "run.completed",
            "session.totals_updated"
        ]
    );
    assert_model_text_delta(&live_after_release[0], "Season");
    wait_for_run_status(&server, run_id.as_str(), RunStatus::Completed);
    drop(server);

    let store = SqliteSessionStore::open(db.path()).expect("store should reopen after completion");
    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should read after completion");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].1[0].part, text_part("hello Season"));
}

#[test]
fn session_send_message_publishes_provider_error_before_run_failed() {
    let db = TestSessionDb::new("provider-error-journal");
    let provider = FakeProviderServer::start(
        429,
        "application/json",
        vec![
            r#"{"error":{"message":"rate limit exceeded","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#
                .to_string(),
        ],
    );
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
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
            "session.totals_updated",
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
        server.run_status(&RunId::try_new(&run_id).unwrap()),
        Some(RunStatus::Failed)
    );
    drop(server);

    let store = SqliteSessionStore::open(db.path()).expect("store should reopen");
    let run_id = RunId::try_new(run_id).expect("run id should parse");
    let payloads = store
        .list_provider_payloads_for_run(&run_id)
        .expect("provider payloads should list");
    let directions = payloads
        .iter()
        .map(|payload| payload.direction.as_str())
        .collect::<Vec<_>>();
    assert_eq!(directions, vec!["request", "error"]);
    assert_eq!(payloads[1].decode_status, "pending");

    let error_payload: Value =
        serde_json::from_slice(&artifact_bytes(&store, &payloads[1].artifact_id))
            .expect("error artifact should be JSON");
    assert_eq!(error_payload["status"], 429);
    assert_eq!(error_payload["message"], "rate limit exceeded");
    assert_eq!(error_payload["error_type"], "rate_limit_error");
    assert_eq!(error_payload["code"], "rate_limit_exceeded");
}

#[test]
fn stream_error_flushes_buffered_deltas_before_error_payload() {
    let db = TestSessionDb::new("stream-error-flush");
    let provider = FakeProviderServer::start(
        200,
        "text/event-stream",
        vec![provider_sse_chunk(
            r#"{"id":"provider-run","model":"vendor/model-large","choices":[{"index":0,"delta":{"content":"partial reply"},"finish_reason":null}]}"#,
        )],
    );
    let config = HttpServerConfig {
        session_db_path: Some(db.path().to_path_buf()),
        ..HttpServerConfig::default()
    };
    let mut server =
        HttpServer::with_model_settings(config, model_settings_with_base_url(provider.base_url()));
    let session_id = create_session(&mut server);

    let run_id = send_message_text(&mut server, &session_id, "stream then fail");

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
            "model.text_delta",
            "provider.error",
            "run.failed",
            "session.totals_updated",
        ]
    );
    drop(server);

    let store = SqliteSessionStore::open(db.path()).expect("store should reopen");
    let run_id = RunId::try_new(run_id).expect("run id should parse");
    let turns = store
        .list_turns_for_run(&run_id)
        .expect("turns should be readable");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].1[0].part, text_part("stream then fail"));
    assert_eq!(turns[1].1[0].part, text_part("partial reply"));

    let payloads = store
        .list_provider_payloads_for_run(&run_id)
        .expect("provider payloads should list");
    assert_eq!(
        payloads
            .iter()
            .map(|payload| payload.direction.as_str())
            .collect::<Vec<_>>(),
        vec!["request", "stream_batch", "error"]
    );
    assert_eq!(payloads[1].decode_status, "decoded");
    assert_eq!(payloads[2].decode_status, "pending");
    let flushed_parts = store
        .list_parts_for_provider_payload(&payloads[1].id)
        .expect("flushed stream payload parts should list");
    assert_eq!(flushed_parts.len(), 1);
    assert_eq!(flushed_parts[0].part, text_part("partial reply"));
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
        vec![
            "session.created",
            "run.started",
            "run.failed",
            "session.totals_updated"
        ]
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
    let event_names_without_totals = event_names(&events)
        .into_iter()
        .filter(|name| *name != "session.totals_updated")
        .collect::<Vec<_>>();
    assert_eq!(
        event_names_without_totals,
        vec!["session.created", "run.started", "run.cancelled"]
    );
    let cancelled_event = events
        .iter()
        .find(|event| event.name == "run.cancelled")
        .expect("run.cancelled event should be published");
    assert_eq!(cancelled_event.data["run_id"], run_id);

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
    let live_events = receive_live_events(&subscription, 5);

    assert_eq!(
        envelope_event_names(&live_events),
        vec![
            "run.started",
            "model.text_delta",
            "message.completed",
            "run.completed",
            "session.totals_updated",
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
        vec![
            "session.created",
            "run.started",
            "run.failed",
            "session.totals_updated"
        ]
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

fn coding_tool_names() -> Vec<&'static str> {
    vec!["bash", "edit", "read", "write"]
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
        .with_tool_registry(bash_tool_registry())
        .with_guardrails(bash_confirmation_guardrails());
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

fn bash_confirmation_guardrails() -> GuardrailRunner {
    let mut guardrails = GuardrailRunner::default();
    guardrails
        .register_hook(BashConfirmationHook)
        .expect("bash confirmation hook should register");
    guardrails
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

struct TestSessionDb {
    path: PathBuf,
}

impl TestSessionDb {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("nav-server-{name}-{}.db", std::process::id()));
        remove_sqlite_files(&path);
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestSessionDb {
    fn drop(&mut self) {
        remove_sqlite_files(&self.path);
    }
}

fn remove_sqlite_files(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_extension("db-wal"));
    let _ = fs::remove_file(path.with_extension("db-shm"));
}

fn artifact_bytes(store: &SqliteSessionStore, artifact_id: &nav_types::ArtifactId) -> Vec<u8> {
    let mut artifact = store
        .get_artifact(artifact_id)
        .expect("artifact should be readable");
    let mut bytes = Vec::new();
    artifact
        .reader
        .read_to_end(&mut bytes)
        .expect("artifact bytes should read");
    bytes
}

fn text_part(text: impl Into<String>) -> Part {
    Part::Text {
        text: text.into(),
        synthetic: None,
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
