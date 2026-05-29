//! Provider-agnostic context-limit error classification (issue #363, CMP-06a).

use nav_harness::models::{
    ApiKind, ContextLimitError, OpenAiCompletionsError, OpenAiCompletionsResponseParser,
    classify_context_limit,
};

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/context-limit/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read fixture {path}: {error}"))
}

#[test]
fn classifies_openai_completions_context_length_error() {
    let body = fixture("openai-completions.json");

    let classified = classify_context_limit(ApiKind::OpenAiCompletions, 400, &body)
        .expect("openai context-length error should classify");

    assert_eq!(
        classified,
        ContextLimitError {
            api: ApiKind::OpenAiCompletions,
            status: 400,
            message:
                "This model's maximum context length is 128000 tokens. However, your messages \
                 resulted in 145231 tokens. Please reduce the length of the messages."
                    .to_string(),
            code: Some("context_length_exceeded".to_string()),
        }
    );
}

#[test]
fn unrelated_openai_400_does_not_classify() {
    let body = fixture("unrelated-openai-400.json");

    assert!(classify_context_limit(ApiKind::OpenAiCompletions, 400, &body).is_none());
}

#[test]
fn unrelated_anthropic_400_does_not_classify() {
    let body = fixture("unrelated-anthropic-400.json");

    assert!(classify_context_limit(ApiKind::AnthropicMessages, 400, &body).is_none());
}

#[test]
fn non_400_context_length_error_does_not_classify() {
    // A 429 carrying a context-flavored message is a rate limit, not overflow.
    let body = fixture("openai-completions.json");

    assert!(classify_context_limit(ApiKind::OpenAiCompletions, 429, &body).is_none());
}

#[test]
fn classifies_openai_responses_context_window_error() {
    let body = fixture("openai-responses.json");

    let classified = classify_context_limit(ApiKind::OpenAiResponses, 400, &body)
        .expect("openai responses context-window error should classify");

    assert_eq!(classified.api, ApiKind::OpenAiResponses);
    assert_eq!(classified.code.as_deref(), Some("context_length_exceeded"));
    assert!(classified.message.contains("context window"));
}

#[test]
fn classifies_bare_context_length_message_without_code() {
    // Some gateways drop `error.code` and return only the bare wording.
    let body = fixture("openai-bare-message.json");

    let classified = classify_context_limit(ApiKind::OpenAiCompletions, 400, &body)
        .expect("bare context-length wording should classify");

    assert_eq!(classified.message, "context length exceeded");
    assert_eq!(classified.code, None);
}

#[test]
fn classifies_openai_gateway_error_nested_under_error_key() {
    // OpenAI-compatible gateways sometimes nest the human-readable text under
    // `error.error` (a string) rather than `error.message`.
    let body = fixture("openai-gateway-nested-error.json");

    let classified = classify_context_limit(ApiKind::OpenAiCompletions, 400, &body)
        .expect("gateway error nested under error.error should classify");

    assert_eq!(
        classified.message,
        "This model's maximum context length is 8192 tokens. However, your request requires more."
    );
    assert_eq!(classified.code, None);
}

#[test]
fn classifies_chatgpt_subscription_context_length_error() {
    let body = fixture("chatgpt-subscription.json");

    let classified = classify_context_limit(ApiKind::ChatGptSubscription, 400, &body)
        .expect("chatgpt subscription context-length error should classify");

    assert_eq!(classified.api, ApiKind::ChatGptSubscription);
    assert_eq!(classified.code.as_deref(), Some("context_length_exceeded"));
}

#[test]
fn classifies_anthropic_prompt_too_long_error() {
    let body = fixture("anthropic-messages.json");

    let classified = classify_context_limit(ApiKind::AnthropicMessages, 400, &body)
        .expect("anthropic prompt-too-long error should classify");

    assert_eq!(
        classified,
        ContextLimitError {
            api: ApiKind::AnthropicMessages,
            status: 400,
            message: "prompt is too long: 215000 tokens > 200000 maximum".to_string(),
            code: None,
        }
    );
}

#[test]
fn completions_provider_layer_surfaces_typed_context_limit_variant() {
    let body = fixture("openai-completions.json");

    let error = OpenAiCompletionsResponseParser::parse_error_response(400, &body, "sk-secret");

    match error {
        OpenAiCompletionsError::ContextLimit(context_limit) => {
            assert_eq!(context_limit.api, ApiKind::OpenAiCompletions);
            assert_eq!(context_limit.status, 400);
            assert_eq!(
                context_limit.code.as_deref(),
                Some("context_length_exceeded")
            );
        }
        other => panic!("expected ContextLimit variant, got {other:?}"),
    }
}

#[test]
fn completions_provider_layer_keeps_unrelated_400_as_provider_error() {
    let body = fixture("unrelated-openai-400.json");

    let error = OpenAiCompletionsResponseParser::parse_error_response(400, &body, "sk-secret");

    assert!(matches!(error, OpenAiCompletionsError::Provider(_)));
}
