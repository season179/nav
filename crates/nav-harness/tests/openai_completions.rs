use std::collections::BTreeMap;
use std::future::Future;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use nav_harness::events::{
    HarnessEvent, HarnessEventEnvelope, HarnessEventIdSource, ModelOutputContext,
};
use nav_harness::models::{
    ApiKeyConfig, ApiKind, ChatCompletionMessageRole, ChatCompletionRequestMessage,
    ChatCompletionStreamEvent, MaxTokensField, ModelConfig, ModelInput, ModelRef, ModelResolver,
    ModelSettings, OpenAiCompletionsCancellationToken, OpenAiCompletionsClient,
    OpenAiCompletionsError, OpenAiCompletionsRequest, OpenAiCompletionsRequestContext,
    OpenAiCompletionsResponseParser, ProviderCompat, ProviderConfig, ProviderRoutingCompat,
    ReasoningEffort, ResolveModelError, ThinkingFormat,
};
use nav_types::{EventId, MessageId, RunId, ToolCallId};
use serde_json::json;

#[test]
fn builds_request_from_resolved_inline_key_without_exposing_secret() {
    let resolved = ModelResolver::new(settings_for(
        "compatible-gateway",
        "https://llm.example.com/v1",
        ApiKeyConfig::Inline {
            inline: "sk-inline-secret".to_string(),
        },
        ProviderCompat {
            supports_reasoning_effort: Some(true),
            supports_usage_in_streaming: Some(true),
            ..Default::default()
        },
    ))
    .resolve_default()
    .expect("inline key should resolve");

    let request = OpenAiCompletionsRequest {
        messages: vec![
            ChatCompletionRequestMessage::system("You are concise."),
            ChatCompletionRequestMessage::user("Say hi"),
        ],
        max_tokens: Some(128),
        temperature: Some(0.2),
        reasoning_effort: Some(ReasoningEffort::Low),
        stream: true,
    };

    let plan = OpenAiCompletionsClient::new()
        .build_request(&resolved, &request)
        .expect("request should build");

    assert_eq!(plan.endpoint, "https://llm.example.com/v1/chat/completions");
    assert_eq!(plan.body["model"], "vendor/model-large");
    assert_eq!(plan.body["messages"][0]["role"], "system");
    assert_eq!(plan.body["messages"][1]["role"], "user");
    assert_eq!(plan.body["max_completion_tokens"], 128);
    assert_eq!(plan.body["temperature"], json!(0.2));
    assert_eq!(
        plan.body["stream_options"],
        json!({ "include_usage": true })
    );
    assert_eq!(plan.body["reasoning_effort"], "low");

    let debug = format!("{plan:?}");
    assert!(!debug.contains("sk-inline-secret"));
}

#[test]
fn omits_stream_usage_options_unless_compat_explicitly_enables_them() {
    let resolved = ModelResolver::new(settings_for(
        "compatible-gateway",
        "https://llm.example.com/v1",
        ApiKeyConfig::Inline {
            inline: "sk-inline-secret".to_string(),
        },
        ProviderCompat::default(),
    ))
    .resolve_default()
    .expect("model should resolve");

    let plan = OpenAiCompletionsClient::new()
        .build_request(
            &resolved,
            &OpenAiCompletionsRequest {
                messages: vec![ChatCompletionRequestMessage::user("Say hi")],
                max_tokens: None,
                temperature: None,
                reasoning_effort: None,
                stream: true,
            },
        )
        .expect("request should build");

    assert!(plan.body.get("stream_options").is_none());
}

#[test]
fn builds_request_for_custom_endpoint_with_env_key_and_compat_quirks() {
    let env_var = "LOCAL_GATEWAY_API_KEY";
    let resolved = ModelResolver::new(settings_for(
        "local-gateway",
        "http://localhost:11434/v1/",
        ApiKeyConfig::EnvVar {
            env_var: env_var.to_string(),
        },
        ProviderCompat {
            supports_developer_role: Some(false),
            supports_usage_in_streaming: Some(false),
            max_tokens_field: Some(MaxTokensField::MaxTokens),
            thinking_format: Some(ThinkingFormat::QwenChatTemplate),
            routing: Some(ProviderRoutingCompat {
                allow_fallbacks: Some(false),
                only: Some(vec!["local".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        },
    ))
    .resolve_default_with_env(|name| (name == env_var).then(|| "sk-env-secret".to_string()))
    .expect("env key should resolve");

    let plan = OpenAiCompletionsClient::new()
        .build_request(
            &resolved,
            &OpenAiCompletionsRequest {
                messages: vec![
                    ChatCompletionRequestMessage::system("Use local rules."),
                    ChatCompletionRequestMessage::user("Say hi"),
                ],
                max_tokens: Some(64),
                temperature: None,
                reasoning_effort: Some(ReasoningEffort::Medium),
                stream: true,
            },
        )
        .expect("request should build");

    assert_eq!(plan.endpoint, "http://localhost:11434/v1/chat/completions");
    assert_eq!(plan.body["messages"][0]["role"], "system");
    assert_eq!(plan.body["max_tokens"], 64);
    assert!(plan.body.get("max_completion_tokens").is_none());
    assert!(plan.body.get("stream_options").is_none());
    assert_eq!(
        plan.body["chat_template_kwargs"],
        json!({
            "enable_thinking": true,
            "preserve_thinking": true,
        })
    );
    assert_eq!(
        plan.body["provider"],
        json!({
            "allow_fallbacks": false,
            "only": ["local"],
        })
    );
    assert!(!format!("{plan:?}").contains("sk-env-secret"));
}

#[test]
fn can_use_developer_role_for_non_reasoning_models_when_compat_allows_it() {
    let mut resolved = ModelResolver::new(settings_for(
        "openai-compatible",
        "https://api.example.com/v1",
        ApiKeyConfig::Inline {
            inline: "sk-test".to_string(),
        },
        ProviderCompat {
            supports_developer_role: Some(true),
            ..Default::default()
        },
    ))
    .resolve_default()
    .expect("model should resolve");
    resolved.model.reasoning = false;

    let plan = OpenAiCompletionsClient::new()
        .build_request(
            &resolved,
            &OpenAiCompletionsRequest::new(vec![ChatCompletionRequestMessage {
                role: ChatCompletionMessageRole::System,
                content: "Use developer rules.".to_string(),
            }]),
        )
        .expect("request should build");

    assert_eq!(plan.body["messages"][0]["role"], "developer");
}

#[test]
fn complete_rejects_streaming_requests_before_http() {
    let resolved = ModelResolver::new(settings_for(
        "compatible-gateway",
        "https://llm.example.com/v1",
        ApiKeyConfig::Inline {
            inline: "sk-test".to_string(),
        },
        ProviderCompat::default(),
    ))
    .resolve_default()
    .expect("model should resolve");

    let result = poll_ready(OpenAiCompletionsClient::new().complete(
        &resolved,
        &OpenAiCompletionsRequest {
            messages: vec![ChatCompletionRequestMessage::user("Say hi")],
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
            stream: true,
        },
    ));

    assert_eq!(result, Err(OpenAiCompletionsError::StreamingUnsupported));
}

#[tokio::test]
async fn stream_events_posts_streaming_request_and_maps_provider_deltas() {
    let (result, request, events) = run_stream_events(FakeSseServer::start(
        200,
        "text/event-stream",
        vec![
            sse_data(
                r#"{"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"content":"hel"},"finish_reason":null}]}"#,
            ),
            sse_data(
                r#"{"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"reasoning_content":"thinking"},"finish_reason":null}]}"#,
            ),
            sse_data(
                r#"{"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_provider_1","type":"function","function":{"name":"shell","arguments":"{\"cmd\""}}]},"finish_reason":null}]}"#,
            ),
            sse_data(
                r#"{"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"ls\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            ),
            sse_data("[DONE]"),
        ],
    ))
    .await;

    result.expect("stream should complete");
    assert_eq!(request.request_line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(request.header("authorization"), Some("Bearer sk-secret"));
    assert_eq!(request.header("accept"), Some("text/event-stream"));
    let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(body["model"], "vendor/model-large");
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"][0]["content"], "Say hi");

    assert_eq!(
        event_types(&events),
        vec![
            "model.text_delta",
            "model.reasoning_delta",
            "tool.call_started",
            "tool.call_delta",
            "tool.call_delta",
            "tool.call_completed",
            "message.completed",
            "run.completed",
        ]
    );

    match &events[0].event {
        HarnessEvent::ModelTextDelta { delta, .. } => assert_eq!(delta, "hel"),
        event => panic!("expected text delta, got {event:?}"),
    }
    match &events[1].event {
        HarnessEvent::ModelReasoningDelta { delta, .. } => assert_eq!(delta, "thinking"),
        event => panic!("expected reasoning delta, got {event:?}"),
    }
    match &events[5].event {
        HarnessEvent::ToolCallCompleted {
            name, arguments, ..
        } => {
            assert_eq!(name.as_deref(), Some("shell"));
            assert_eq!(arguments, r#"{"cmd":"ls"}"#);
        }
        event => panic!("expected tool completion, got {event:?}"),
    }
}

#[tokio::test]
async fn stream_events_maps_provider_stream_error_and_redacts_key() {
    let (result, _, events) = run_stream_events(FakeSseServer::start(
        200,
        "text/event-stream",
        vec![sse_data(
            r#"{"error":{"message":"bad key sk-secret","type":"authentication_error","code":"invalid_api_key"}}"#,
        )],
    ))
    .await;

    let error = result.expect_err("provider stream error should fail the stream call");
    assert_eq!(
        error,
        OpenAiCompletionsError::ProviderStream(
            nav_harness::models::OpenAiCompletionsStreamProviderError {
                message: "bad key <redacted>".to_string(),
                error_type: Some("authentication_error".to_string()),
                code: Some("invalid_api_key".to_string()),
            },
        )
    );
    assert_eq!(event_types(&events), vec!["provider.error"]);
    match &events[0].event {
        HarnessEvent::ProviderError {
            status,
            message,
            error_type,
            code,
            ..
        } => {
            assert_eq!(*status, None);
            assert_eq!(message, "bad key <redacted>");
            assert_eq!(error_type.as_deref(), Some("authentication_error"));
            assert_eq!(code.as_deref(), Some("invalid_api_key"));
        }
        event => panic!("expected provider error, got {event:?}"),
    }
    assert!(!format!("{events:?}").contains("sk-secret"));
}

#[tokio::test]
async fn stream_events_buffers_sse_events_split_across_network_chunks() {
    let (result, _, events) = run_stream_events(FakeSseServer::start(
        200,
        "text/event-stream",
        vec![
            br#"data: {"id":"chatcmpl_1","choices":[{"delta":{"content":"hel"},"finish_reason":null"#.to_vec(),
            br#"}]}"#.to_vec(),
            b"\n\n".to_vec(),
            sse_data("[DONE]"),
        ],
    ))
    .await;

    result.expect("split SSE event should complete");
    assert_eq!(
        event_types(&events),
        vec!["model.text_delta", "run.completed"]
    );
    match &events[0].event {
        HarnessEvent::ModelTextDelta { delta, .. } => assert_eq!(delta, "hel"),
        event => panic!("expected text delta, got {event:?}"),
    }
}

#[tokio::test]
async fn stream_events_maps_malformed_sse_data_to_provider_error() {
    let (result, _, events) = run_stream_events(FakeSseServer::start(
        200,
        "text/event-stream",
        vec![sse_data(
            r#"{"choices":[{"delta":{"content":"sk-secret"}}]"#,
        )],
    ))
    .await;

    let error = result.expect_err("malformed SSE data should fail the stream call");
    assert_eq!(event_types(&events), vec!["provider.error"]);
    assert!(!format!("{error:?}").contains("sk-secret"));
    match &events[0].event {
        HarnessEvent::ProviderError { message, .. } => {
            assert!(message.contains("malformed provider response"));
            assert!(!message.contains("sk-secret"));
        }
        event => panic!("expected provider error, got {event:?}"),
    }
}

#[tokio::test]
async fn stream_events_maps_eof_before_done_to_provider_error() {
    let (result, _, events) = run_stream_events(FakeSseServer::start(
        200,
        "text/event-stream",
        vec![sse_data(
            r#"{"id":"chatcmpl_1","choices":[{"delta":{"content":"hel"},"finish_reason":null}]}"#,
        )],
    ))
    .await;

    let error = result.expect_err("EOF before [DONE] should fail the stream call");
    assert_eq!(
        error,
        OpenAiCompletionsError::MalformedResponse {
            message: "stream ended before [DONE]".to_string(),
        }
    );
    assert_eq!(
        event_types(&events),
        vec!["model.text_delta", "provider.error"]
    );
    match &events[1].event {
        HarnessEvent::ProviderError { message, .. } => {
            assert_eq!(
                message,
                "malformed provider response: stream ended before [DONE]"
            );
        }
        event => panic!("expected provider error, got {event:?}"),
    }
}

#[tokio::test]
async fn stream_events_preserves_http_provider_error_parsing() {
    let (result, _, events) = run_stream_events(FakeSseServer::start(
        401,
        "application/json",
        vec![br#"{"error":{"message":"bad key sk-secret","type":"authentication_error","code":"invalid_api_key"}}"#.to_vec()],
    ))
    .await;

    let error = result.expect_err("HTTP provider error should fail the stream call");
    assert!(events.is_empty());
    assert_eq!(
        error,
        OpenAiCompletionsError::Provider(nav_harness::models::OpenAiCompletionsProviderError {
            status: 401,
            message: "bad key <redacted>".to_string(),
            error_type: Some("authentication_error".to_string()),
            code: Some("invalid_api_key".to_string()),
        })
    );
    assert!(!format!("{error:?}").contains("sk-secret"));
}

#[tokio::test]
async fn stream_events_honors_cancelled_request_context_before_http() {
    let resolved = resolved_model("http://127.0.0.1:9/v1");
    let token = OpenAiCompletionsCancellationToken::new();
    token.cancel();
    let request_context = OpenAiCompletionsRequestContext::new().with_cancellation_token(token);
    let mut ids = TestIds::default();
    let mut events = Vec::new();

    let error = OpenAiCompletionsClient::new()
        .stream_events_with_context(
            &resolved,
            &OpenAiCompletionsRequest::from_user("Say hi"),
            &request_context,
            model_output_context(),
            &mut ids,
            |batch| events.extend(batch),
        )
        .await
        .expect_err("cancelled request should not start transport");

    assert_eq!(error, OpenAiCompletionsError::Cancelled);
    assert!(events.is_empty());
}

#[tokio::test]
async fn stream_events_honors_cancelled_request_context_between_batches() {
    let server = FakeSseServer::start_with_chunk_delay(
        200,
        "text/event-stream",
        vec![
            sse_data(
                r#"{"id":"chatcmpl_1","choices":[{"delta":{"content":"hel"},"finish_reason":null}]}"#,
            ),
            sse_data(
                r#"{"id":"chatcmpl_1","choices":[{"delta":{"content":"lo"},"finish_reason":null}]}"#,
            ),
            sse_data("[DONE]"),
        ],
        Duration::from_millis(20),
    );
    let resolved = resolved_model(server.base_url());
    let token = OpenAiCompletionsCancellationToken::new();
    let request_context =
        OpenAiCompletionsRequestContext::new().with_cancellation_token(token.clone());
    let mut ids = TestIds::default();
    let mut events = Vec::new();

    let error = OpenAiCompletionsClient::new()
        .stream_events_with_context(
            &resolved,
            &OpenAiCompletionsRequest::from_user("Say hi"),
            &request_context,
            model_output_context(),
            &mut ids,
            |batch| {
                events.extend(batch);
                token.cancel();
            },
        )
        .await
        .expect_err("cancelled request should stop the stream call");

    server.join();
    assert_eq!(error, OpenAiCompletionsError::Cancelled);
    assert_eq!(event_types(&events), vec!["model.text_delta"]);
    match &events[0].event {
        HarnessEvent::ModelTextDelta { delta, .. } => assert_eq!(delta, "hel"),
        event => panic!("expected text delta, got {event:?}"),
    }
}

#[tokio::test]
async fn stream_events_cancels_while_waiting_for_next_provider_chunk() {
    let server = FakeSseServer::start_with_chunk_delay(
        200,
        "text/event-stream",
        vec![
            Vec::new(),
            sse_data(
                r#"{"id":"chatcmpl_1","choices":[{"delta":{"content":"hel"},"finish_reason":null}]}"#,
            ),
        ],
        Duration::from_millis(250),
    );
    let resolved = resolved_model(server.base_url());
    let token = OpenAiCompletionsCancellationToken::new();
    let cancel_from_thread = token.clone();
    let request_context = OpenAiCompletionsRequestContext::new().with_cancellation_token(token);
    let mut ids = TestIds::default();
    let mut events = Vec::new();

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        cancel_from_thread.cancel();
    });

    let result = tokio::time::timeout(
        Duration::from_millis(150),
        OpenAiCompletionsClient::new().stream_events_with_context(
            &resolved,
            &OpenAiCompletionsRequest::from_user("Say hi"),
            &request_context,
            model_output_context(),
            &mut ids,
            |batch| events.extend(batch),
        ),
    )
    .await
    .expect("cancel should interrupt blocked chunk read");

    server.join();
    assert_eq!(result, Err(OpenAiCompletionsError::Cancelled));
    assert!(events.is_empty());
}

#[test]
fn reports_missing_key_from_resolver_as_client_error() {
    let error: OpenAiCompletionsError = ModelResolver::new(settings_for(
        "compatible-gateway",
        "https://llm.example.com/v1",
        ApiKeyConfig::EnvVar {
            env_var: "MISSING_KEY".to_string(),
        },
        ProviderCompat::default(),
    ))
    .resolve_default_with_env(|_| None)
    .expect_err("missing key should fail")
    .into();

    assert_eq!(
        error,
        OpenAiCompletionsError::MissingApiKey {
            provider_id: "compatible-gateway".to_string(),
            env_var: Some("MISSING_KEY".to_string()),
        }
    );
}

#[test]
fn preserves_non_key_resolver_errors_as_model_resolution_errors() {
    let error: OpenAiCompletionsError = ModelResolver::new(settings_for(
        "compatible-gateway",
        "https://llm.example.com/v1",
        ApiKeyConfig::Inline {
            inline: "sk-test".to_string(),
        },
        ProviderCompat::default(),
    ))
    .resolve("missing-provider", "vendor/model-large")
    .expect_err("unknown provider should fail")
    .into();

    assert_eq!(
        error,
        OpenAiCompletionsError::ModelResolution {
            error: ResolveModelError::UnknownProvider {
                provider_id: "missing-provider".to_string(),
            },
        }
    );
    assert!(!error.to_string().contains("malformed provider response"));
}

#[test]
fn parses_provider_error_and_redacts_api_key() {
    let error = OpenAiCompletionsResponseParser::parse_error_response(
        401,
        r#"{"error":{"message":"bad key sk-secret","type":"authentication_error","code":"invalid_api_key"}}"#,
        "sk-secret",
    );

    assert_eq!(
        error,
        OpenAiCompletionsError::Provider(nav_harness::models::OpenAiCompletionsProviderError {
            status: 401,
            message: "bad key <redacted>".to_string(),
            error_type: Some("authentication_error".to_string()),
            code: Some("invalid_api_key".to_string()),
        })
    );
    assert!(!format!("{error:?}").contains("sk-secret"));
    assert!(!error.to_string().contains("sk-secret"));
}

#[test]
fn preserves_plain_http_error_without_leaking_key() {
    let error = OpenAiCompletionsResponseParser::parse_error_response(
        502,
        "upstream included sk-secret in diagnostics",
        "sk-secret",
    );

    assert_eq!(
        error,
        OpenAiCompletionsError::Http {
            status: 502,
            body: "upstream included <redacted> in diagnostics".to_string(),
        }
    );
}

#[test]
fn parses_non_streaming_and_streaming_response_boundaries() {
    let response = OpenAiCompletionsResponseParser::parse_response(
            r#"{"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
            "sk-secret",
        )
        .expect("response should parse");
    assert_eq!(
        response.choices[0].message.content.as_deref(),
        Some("hello")
    );
    assert_eq!(
        response.usage.expect("usage should parse").total_tokens,
        Some(4)
    );

    let chunk = OpenAiCompletionsResponseParser::parse_stream_chunk(
            r#"{"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"role":"assistant","content":"hel"},"finish_reason":null}]}"#,
            "sk-secret",
        )
        .expect("stream chunk should parse");
    assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hel"));
}

#[test]
fn parses_openai_compatible_sse_stream_events() {
    let chunk = OpenAiCompletionsResponseParser::parse_stream_event(
        "event: message\ndata: {\"id\":\"chatcmpl_1\",\"choices\":[{\"delta\":{\"content\":\"hel\"},\"finish_reason\":null}]}\n\n",
        "sk-secret",
    )
    .expect("SSE event should parse")
    .expect("data event should produce a chunk");

    assert_eq!(
        chunk,
        ChatCompletionStreamEvent::Chunk(nav_harness::models::ChatCompletionStreamChunk {
            id: Some("chatcmpl_1".to_string()),
            model: None,
            choices: vec![nav_harness::models::ChatCompletionStreamChoice {
                index: None,
                delta: nav_harness::models::ChatCompletionDelta {
                    role: None,
                    content: Some("hel".to_string()),
                    reasoning_content: None,
                    reasoning: None,
                    reasoning_text: None,
                    tool_calls: Vec::new(),
                },
                finish_reason: None,
            }],
            usage: None,
        })
    );

    assert_eq!(
        OpenAiCompletionsResponseParser::parse_stream_event("data: [DONE]", "sk-secret")
            .expect("done event should parse"),
        Some(ChatCompletionStreamEvent::Done)
    );
    assert_eq!(
        OpenAiCompletionsResponseParser::parse_stream_event(": keepalive", "sk-secret")
            .expect("comment should parse"),
        None
    );
}

#[test]
fn malformed_response_is_typed() {
    let error = OpenAiCompletionsResponseParser::parse_response(r#"{"choices":[]}"#, "sk-secret")
        .expect_err("empty choices should fail");

    assert_eq!(
        error,
        OpenAiCompletionsError::MalformedResponse {
            message: "response did not include any choices".to_string(),
        }
    );
}

#[derive(Default)]
struct TestIds {
    next_event: u64,
    next_tool_call: u64,
}

impl HarnessEventIdSource for TestIds {
    fn next_event_id(&mut self) -> EventId {
        self.next_event += 1;
        event_id(self.next_event)
    }

    fn next_tool_call_id(&mut self) -> ToolCallId {
        self.next_tool_call += 1;
        tool_call_id(self.next_tool_call)
    }
}

fn resolved_model(base_url: &str) -> nav_harness::models::ResolvedModelConfig {
    ModelResolver::new(settings_for(
        "compatible-gateway",
        base_url,
        ApiKeyConfig::Inline {
            inline: "sk-secret".to_string(),
        },
        ProviderCompat::default(),
    ))
    .resolve_default()
    .expect("model should resolve")
}

fn model_output_context() -> ModelOutputContext {
    ModelOutputContext {
        run_id: run_id(),
        message_id: message_id(),
        provider_id: "compatible-gateway".to_string(),
        configured_model_id: "vendor/model-large".to_string(),
    }
}

fn event_types(events: &[HarnessEventEnvelope]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| event.event.event_type())
        .collect()
}

async fn run_stream_events(
    server: FakeSseServer,
) -> (
    Result<(), OpenAiCompletionsError>,
    FakeHttpRequest,
    Vec<HarnessEventEnvelope>,
) {
    let resolved = resolved_model(server.base_url());
    let mut ids = TestIds::default();
    let mut events = Vec::new();

    let result = OpenAiCompletionsClient::new()
        .stream_events(
            &resolved,
            &OpenAiCompletionsRequest::from_user("Say hi"),
            model_output_context(),
            &mut ids,
            |batch| events.extend(batch),
        )
        .await;
    let request = server.join();

    (result, request, events)
}

fn sse_data(data: &str) -> Vec<u8> {
    format!("data: {data}\n\n").into_bytes()
}

fn event_id(index: u64) -> EventId {
    EventId::try_new(format!("019f2f6f-f178-7a72-9f28-{index:012x}"))
        .expect("test event id should be UUIDv7-shaped")
}

fn tool_call_id(index: u64) -> ToolCallId {
    ToolCallId::try_new(format!("019f2f6f-f178-7a72-9f28-{index:012x}"))
        .expect("test tool call id should be UUIDv7-shaped")
}

fn run_id() -> RunId {
    RunId::try_new("019f2f6f-f178-7a72-9f28-000000000101")
        .expect("test run id should be UUIDv7-shaped")
}

fn message_id() -> MessageId {
    MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000202")
        .expect("test message id should be UUIDv7-shaped")
}

struct FakeSseServer {
    base_url: String,
    handle: JoinHandle<FakeHttpRequest>,
}

impl FakeSseServer {
    fn start(status: u16, content_type: &'static str, body_chunks: Vec<Vec<u8>>) -> Self {
        Self::start_with_optional_chunk_delay(status, content_type, body_chunks, None)
    }

    fn start_with_chunk_delay(
        status: u16,
        content_type: &'static str,
        body_chunks: Vec<Vec<u8>>,
        chunk_delay: Duration,
    ) -> Self {
        Self::start_with_optional_chunk_delay(status, content_type, body_chunks, Some(chunk_delay))
    }

    fn start_with_optional_chunk_delay(
        status: u16,
        content_type: &'static str,
        body_chunks: Vec<Vec<u8>>,
        chunk_delay: Option<Duration>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake server should bind");
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("fake server should accept");
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("request line should read");

            let mut headers = Vec::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader
                    .read_line(&mut line)
                    .expect("request header should read");
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    content_length = value.trim().parse().unwrap();
                }
                headers.push(line.trim_end().to_string());
            }

            let mut body_bytes = vec![0; content_length];
            reader
                .read_exact(&mut body_bytes)
                .expect("request body should read");

            let mut stream = reader.into_inner();
            write!(
                stream,
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                status,
                reason_phrase(status),
                content_type
            )
            .expect("response headers should write");
            for chunk in body_chunks {
                if stop_sending_on_broken_pipe(
                    stream.write_all(&chunk),
                    "response chunk should write",
                ) {
                    break;
                }
                if stop_sending_on_broken_pipe(stream.flush(), "response chunk should flush") {
                    break;
                }
                if let Some(chunk_delay) = chunk_delay {
                    thread::sleep(chunk_delay);
                }
            }

            FakeHttpRequest {
                request_line: request_line.trim_end().to_string(),
                headers,
                body: String::from_utf8(body_bytes).expect("request body should be UTF-8"),
            }
        });

        Self { base_url, handle }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn join(self) -> FakeHttpRequest {
        self.handle.join().expect("fake server should finish")
    }
}

fn stop_sending_on_broken_pipe(result: std::io::Result<()>, context: &str) -> bool {
    match result {
        Ok(()) => false,
        Err(error) if error.kind() == ErrorKind::BrokenPipe => true,
        Err(error) => panic!("{context}: {error}"),
    }
}

struct FakeHttpRequest {
    request_line: String,
    headers: Vec<String>,
    body: String,
}

impl FakeHttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find_map(|header| {
            let (header_name, value) = header.split_once(':')?;
            header_name
                .eq_ignore_ascii_case(name)
                .then_some(value.trim())
        })
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        _ => "Error",
    }
}

fn settings_for(
    provider_id: &str,
    base_url: &str,
    api_key: ApiKeyConfig,
    compat: ProviderCompat,
) -> ModelSettings {
    ModelSettings {
        default_model: Some(ModelRef {
            provider: provider_id.to_string(),
            model: "vendor/model-large".to_string(),
        }),
        providers: BTreeMap::from([(
            provider_id.to_string(),
            ProviderConfig {
                name: Some("Compatible".to_string()),
                api: ApiKind::OpenAiCompletions,
                base_url: base_url.to_string(),
                api_key,
                models: vec![ModelConfig {
                    id: "vendor/model-large".to_string(),
                    name: None,
                    api: None,
                    base_url: None,
                    reasoning: true,
                    input: vec![ModelInput::Text],
                    context_window: Some(128000),
                    max_tokens: Some(32000),
                    compat: Default::default(),
                }],
                compat,
            },
        )]),
    }
}

struct NoopWaker;

impl Wake for NoopWaker {
    fn wake(self: Arc<Self>) {}
}

fn poll_ready<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);

    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("future should complete before awaiting"),
    }
}
