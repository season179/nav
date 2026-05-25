use std::collections::BTreeMap;

use nav_harness::models::{
    ApiKeyConfig, ApiKind, ChatCompletionMessageRole, ChatCompletionRequestMessage,
    ChatCompletionStreamEvent, MaxTokensField, ModelConfig, ModelInput, ModelRef, ModelResolver,
    ModelSettings, OpenAiCompletionsClient, OpenAiCompletionsError, OpenAiCompletionsRequest,
    OpenAiCompletionsResponseParser, ProviderCompat, ProviderConfig, ProviderRoutingCompat,
    ReasoningEffort, ResolveModelError, ThinkingFormat,
};
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
fn can_use_developer_role_when_compat_allows_it() {
    let resolved = ModelResolver::new(settings_for(
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
