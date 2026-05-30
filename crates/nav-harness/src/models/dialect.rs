//! Dialect dispatch for the live run loop.
//!
//! The run loop holds canonical `ModelTurn`s and a resolved `ApiKind`. This
//! module turns that pair into a provider-specific request (`EncodedRequest`),
//! the HTTP endpoint to send it to, the wire body, and the auth style — so the
//! loop never hardcodes a single dialect.

use reqwest::Url;
use serde_json::{Map, Value, json};

use crate::context::reminders::ContextReminders;
use crate::sessions::ModelTurn;
use crate::tools::{ToolPreset, ToolRegistry};

use super::encode::{
    AnthropicMessagesEncoder, AnthropicMessagesRequest, Encoder, OpenAiChatCompletionsEncoder,
    OpenAiResponsesEncoder, OpenAiResponsesRequest,
};
use super::{ApiKind, OpenAiCompletionsError, OpenAiCompletionsRequest, ResolvedModelConfig};

/// Anthropic Messages requires `max_tokens`; fall back to this when the model
/// config does not pin one.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// `anthropic-version` header value sent with every Anthropic Messages request.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A request encoded for a specific provider dialect.
///
/// `ChatGptSubscription` is deliberately absent: until subscription-specific
/// transport and journaling land, that `ApiKind` is routed through the Chat
/// Completions encoder/decoder (see [`encode_request`]).
#[derive(Debug, Clone, PartialEq)]
pub enum EncodedRequest {
    Completions(OpenAiCompletionsRequest),
    Responses(OpenAiResponsesRequest),
    Anthropic(AnthropicMessagesRequest),
}

/// Encode canonical turns into the dialect selected by `api`.
///
/// `reminders` are injected as a `<system-reminder>` block in the last user
/// message of the Anthropic dialect (plans/context-management.md §2.3); the
/// OpenAI dialects do not carry the cache-stable system/message split this
/// relies on, so they ignore it for now.
pub fn encode_request(
    api: ApiKind,
    turns: &[ModelTurn],
    tool_registry: &ToolRegistry,
    tool_preset: ToolPreset,
    reminders: &ContextReminders,
) -> EncodedRequest {
    match api {
        ApiKind::OpenAiResponses => {
            let encoder = OpenAiResponsesEncoder::new();
            EncodedRequest::Responses(infallible(Encoder::encode(&encoder, turns)))
        }
        ApiKind::AnthropicMessages => {
            let encoder = AnthropicMessagesEncoder::new()
                .with_tool_registry(tool_registry, tool_preset)
                .with_reminders(reminders.clone());
            EncodedRequest::Anthropic(infallible(Encoder::encode(&encoder, turns)))
        }
        // Chat Completions and (for now) ChatGPT subscription both encode as
        // Chat Completions.
        ApiKind::OpenAiCompletions | ApiKind::ChatGptSubscription => {
            let encoder =
                OpenAiChatCompletionsEncoder::new().with_tool_registry(tool_registry, tool_preset);
            EncodedRequest::Completions(infallible(Encoder::encode(&encoder, turns)))
        }
    }
}

fn infallible<T>(result: Result<T, std::convert::Infallible>) -> T {
    result.unwrap_or_else(|never| match never {})
}

/// How a dialect authenticates its HTTP request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>` (OpenAI Chat Completions / Responses).
    Bearer,
    /// `x-api-key: <key>` plus `anthropic-version` (Anthropic Messages).
    AnthropicApiKey,
}

/// A non-streaming HTTP request for a provider dialect: where to POST, the JSON
/// body to send, and how to authenticate.
#[derive(Debug, Clone, PartialEq)]
pub struct DialectHttpRequest {
    pub endpoint: String,
    pub body: Value,
    pub auth: AuthStyle,
}

/// Build the `POST /responses` request for an OpenAI Responses dialect.
pub fn responses_http_request(
    model: &ResolvedModelConfig,
    request: &OpenAiResponsesRequest,
) -> Result<DialectHttpRequest, OpenAiCompletionsError> {
    let mut body = Map::new();
    body.insert("model".to_string(), json!(model.model.id));
    body.insert("input".to_string(), json!(request.input));
    body.insert("stream".to_string(), json!(false));
    if let Some(instructions) = &request.instructions {
        body.insert("instructions".to_string(), json!(instructions));
    }
    if let Some(previous_response_id) = &request.previous_response_id {
        body.insert(
            "previous_response_id".to_string(),
            json!(previous_response_id),
        );
    }
    if let Some(max_output_tokens) = model.model.max_tokens {
        body.insert("max_output_tokens".to_string(), json!(max_output_tokens));
    }

    Ok(DialectHttpRequest {
        endpoint: join_endpoint(&model.base_url, "responses")?,
        body: Value::Object(body),
        auth: AuthStyle::Bearer,
    })
}

/// Build the `POST /messages` request for an Anthropic Messages dialect.
pub fn anthropic_http_request(
    model: &ResolvedModelConfig,
    request: &AnthropicMessagesRequest,
) -> Result<DialectHttpRequest, OpenAiCompletionsError> {
    // `to_request_body` assembles system/messages/tools with the cache_control
    // breakpoints (plans/context-management.md §2.4); layer the model and the
    // Anthropic-required `max_tokens`/`stream` fields on top.
    let mut body = request.to_request_body();
    let map = body
        .as_object_mut()
        .expect("to_request_body returns a JSON object");
    map.insert("model".to_string(), json!(model.model.id));
    map.insert(
        "max_tokens".to_string(),
        json!(model.model.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
    );
    map.insert("stream".to_string(), json!(false));

    Ok(DialectHttpRequest {
        endpoint: join_endpoint(&model.base_url, "messages")?,
        body,
        auth: AuthStyle::AnthropicApiKey,
    })
}

/// The assistant output extracted from a non-streaming dialect response.
///
/// Carries the provider's own tool-call ids (not canonical ids) so the live
/// loop emits events and dispatches tools exactly as the Chat Completions
/// streaming path does.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtractedTurn {
    pub text: String,
    pub tool_calls: Vec<ExtractedToolCall>,
    pub finish_reason: Option<String>,
    pub provider_response_id: Option<String>,
    pub provider_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedToolCall {
    pub provider_id: String,
    pub name: String,
    pub arguments: String,
}

/// Extract the assistant turn from a decoded non-streaming dialect response.
pub fn extract_turn(api: ApiKind, response: &Value) -> ExtractedTurn {
    match api {
        ApiKind::AnthropicMessages => extract_anthropic_turn(response),
        ApiKind::OpenAiResponses => extract_responses_turn(response),
        // Chat Completions / subscription never take the non-streaming path.
        ApiKind::OpenAiCompletions | ApiKind::ChatGptSubscription => ExtractedTurn::default(),
    }
}

fn extract_anthropic_turn(response: &Value) -> ExtractedTurn {
    let mut text = String::new();
    let mut tool_calls = Vec::new();

    if let Some(content) = response.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                        text.push_str(chunk);
                    }
                }
                Some("tool_use") => {
                    if let Some(tool_call) = anthropic_tool_use(block) {
                        tool_calls.push(tool_call);
                    }
                }
                _ => {}
            }
        }
    }

    ExtractedTurn {
        text,
        tool_calls,
        finish_reason: string_field(response, "stop_reason"),
        provider_response_id: string_field(response, "id"),
        provider_model: string_field(response, "model"),
    }
}

fn anthropic_tool_use(block: &Value) -> Option<ExtractedToolCall> {
    let provider_id = block.get("id").and_then(Value::as_str)?;
    let name = block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = block
        .get("input")
        .map(stringify_arguments)
        .unwrap_or_else(|| "{}".to_string());
    Some(ExtractedToolCall {
        provider_id: provider_id.to_string(),
        name: name.to_string(),
        arguments,
    })
}

fn extract_responses_turn(response: &Value) -> ExtractedTurn {
    let mut text = String::new();
    let mut tool_calls = Vec::new();

    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => push_responses_message_text(item, &mut text),
                Some("function_call") => {
                    if let Some(tool_call) = responses_function_call(item) {
                        tool_calls.push(tool_call);
                    }
                }
                _ => {}
            }
        }
    }

    ExtractedTurn {
        text,
        tool_calls,
        finish_reason: string_field(response, "status"),
        provider_response_id: string_field(response, "id"),
        provider_model: string_field(response, "model"),
    }
}

fn push_responses_message_text(item: &Value, text: &mut String) {
    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return;
    };
    for part in content {
        if part.get("type").and_then(Value::as_str) == Some("output_text")
            && let Some(chunk) = part.get("text").and_then(Value::as_str)
        {
            text.push_str(chunk);
        }
    }
}

fn responses_function_call(item: &Value) -> Option<ExtractedToolCall> {
    let provider_id = item.get("call_id").and_then(Value::as_str)?;
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}")
        .to_string();
    Some(ExtractedToolCall {
        provider_id: provider_id.to_string(),
        name: name.to_string(),
        arguments,
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn stringify_arguments(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Append `path` to a provider `base_url`, preserving any base path segment and
/// dropping query/fragment — mirrors the Chat Completions endpoint builder.
fn join_endpoint(base_url: &str, path: &str) -> Result<String, OpenAiCompletionsError> {
    let mut url = Url::parse(base_url).map_err(|error| OpenAiCompletionsError::InvalidBaseUrl {
        base_url: base_url.to_string(),
        message: error.to_string(),
    })?;

    let base_path = url.path().trim_end_matches('/');
    let endpoint_path = if base_path.is_empty() {
        format!("/{path}")
    } else {
        format!("{base_path}/{path}")
    };
    url.set_path(&endpoint_path);
    url.set_query(None);
    url.set_fragment(None);

    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        AnthropicToolDefinition, ApiKeyConfig, ModelConfig, ProviderConfig, ResolvedApiKey,
    };

    fn registry() -> ToolRegistry {
        ToolRegistry::default()
    }

    fn test_model(api: ApiKind, max_tokens: Option<u32>) -> ResolvedModelConfig {
        let model = ModelConfig {
            id: "model-id".to_string(),
            name: None,
            api: Some(api),
            base_url: None,
            reasoning: false,
            input: Vec::new(),
            context_window: None,
            max_tokens,
            compat: Default::default(),
        };
        let provider = ProviderConfig {
            name: None,
            api,
            base_url: "https://api.test/v1".to_string(),
            api_key: ApiKeyConfig::Inline {
                inline: "secret".to_string(),
            },
            models: vec![model.clone()],
            compat: Default::default(),
        };
        ResolvedModelConfig {
            compat: Default::default(),
            api,
            base_url: "https://api.test/v1".to_string(),
            provider_id: "test-provider".to_string(),
            provider,
            model,
            api_key: ResolvedApiKey::new("secret"),
        }
    }

    #[test]
    fn dispatches_anthropic_messages() {
        let turns = vec![ModelTurn::user_text("hello")];
        let encoded = encode_request(
            ApiKind::AnthropicMessages,
            &turns,
            &registry(),
            ToolPreset::Coding,
            &ContextReminders::new(),
        );
        assert!(matches!(encoded, EncodedRequest::Anthropic(_)));
    }

    #[test]
    fn anthropic_request_injects_context_reminders() {
        let turns = vec![ModelTurn::user_text("hello")];
        let encoded = encode_request(
            ApiKind::AnthropicMessages,
            &turns,
            &registry(),
            ToolPreset::Coding,
            &ContextReminders::new().plan_mode(true),
        );
        let EncodedRequest::Anthropic(request) = encoded else {
            panic!("expected an Anthropic request");
        };
        let content = request.messages.last().unwrap()["content"]
            .as_array()
            .unwrap();
        assert!(content.iter().any(|block| {
            block["text"]
                .as_str()
                .is_some_and(|text| text.contains("[Plan Mode: Active]"))
        }));
    }

    #[test]
    fn dispatches_openai_responses() {
        let turns = vec![ModelTurn::user_text("hello")];
        let encoded = encode_request(
            ApiKind::OpenAiResponses,
            &turns,
            &registry(),
            ToolPreset::Coding,
            &ContextReminders::new(),
        );
        assert!(matches!(encoded, EncodedRequest::Responses(_)));
    }

    #[test]
    fn dispatches_chat_completions() {
        let turns = vec![ModelTurn::user_text("hello")];
        let encoded = encode_request(
            ApiKind::OpenAiCompletions,
            &turns,
            &registry(),
            ToolPreset::Coding,
            &ContextReminders::new(),
        );
        assert!(matches!(encoded, EncodedRequest::Completions(_)));
    }

    #[test]
    fn chatgpt_subscription_routes_through_chat_completions() {
        let turns = vec![ModelTurn::user_text("hello")];
        let encoded = encode_request(
            ApiKind::ChatGptSubscription,
            &turns,
            &registry(),
            ToolPreset::Coding,
            &ContextReminders::new(),
        );
        assert!(matches!(encoded, EncodedRequest::Completions(_)));
    }

    #[test]
    fn anthropic_body_carries_model_messages_system_and_required_max_tokens() {
        let model = test_model(ApiKind::AnthropicMessages, Some(1024));
        let request = AnthropicMessagesRequest {
            system: Some("be helpful".to_string()),
            messages: vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})],
            tools: Vec::new(),
            subagent_fork: false,
        };

        let http = anthropic_http_request(&model, &request).unwrap();

        assert_eq!(http.endpoint, "https://api.test/v1/messages");
        assert_eq!(http.auth, AuthStyle::AnthropicApiKey);
        assert_eq!(http.body["model"], json!("model-id"));
        // System is emitted as cache-annotated text blocks, not a bare string,
        // so the static-system-end breakpoint can ride on Block 1.
        assert_eq!(
            http.body["system"],
            json!([{
                "type": "text",
                "text": "be helpful",
                "cache_control": {"type": "ephemeral"},
            }])
        );
        assert_eq!(http.body["max_tokens"], json!(1024));
        assert_eq!(http.body["stream"], json!(false));
        // The sole message is the last one, so it carries the rolling breakpoint.
        assert_eq!(
            http.body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert!(http.body.get("tools").is_none());
    }

    #[test]
    fn anthropic_body_defaults_max_tokens_when_model_omits_it() {
        let model = test_model(ApiKind::AnthropicMessages, None);
        let request = AnthropicMessagesRequest::new(Vec::new());

        let http = anthropic_http_request(&model, &request).unwrap();

        assert_eq!(http.body["max_tokens"], json!(DEFAULT_MAX_TOKENS));
    }

    #[test]
    fn anthropic_body_includes_tools_when_present() {
        let model = test_model(ApiKind::AnthropicMessages, Some(256));
        let request = AnthropicMessagesRequest {
            system: None,
            messages: Vec::new(),
            tools: vec![AnthropicToolDefinition {
                name: "read".to_string(),
                description: "read a file".to_string(),
                input_schema: json!({"type": "object"}),
            }],
            subagent_fork: false,
        };

        let http = anthropic_http_request(&model, &request).unwrap();

        // The last (here, only) tool definition carries the tools-end breakpoint.
        assert_eq!(
            http.body["tools"],
            json!([{
                "name": "read",
                "description": "read a file",
                "input_schema": {"type": "object"},
                "cache_control": {"type": "ephemeral"},
            }])
        );
    }

    #[test]
    fn responses_body_carries_model_input_instructions_and_no_stream() {
        let model = test_model(ApiKind::OpenAiResponses, Some(2048));
        let request = OpenAiResponsesRequest {
            instructions: Some("system prompt".to_string()),
            input: vec![json!({"type": "message", "role": "user", "content": []})],
            previous_response_id: Some("resp_123".to_string()),
        };

        let http = responses_http_request(&model, &request).unwrap();

        assert_eq!(http.endpoint, "https://api.test/v1/responses");
        assert_eq!(http.auth, AuthStyle::Bearer);
        assert_eq!(http.body["model"], json!("model-id"));
        assert_eq!(http.body["instructions"], json!("system prompt"));
        assert_eq!(http.body["previous_response_id"], json!("resp_123"));
        assert_eq!(http.body["max_output_tokens"], json!(2048));
        assert_eq!(http.body["stream"], json!(false));
        assert_eq!(http.body["input"], json!(request.input));
    }

    #[test]
    fn responses_body_omits_optional_fields_when_absent() {
        let model = test_model(ApiKind::OpenAiResponses, None);
        let request = OpenAiResponsesRequest::new(Vec::new());

        let http = responses_http_request(&model, &request).unwrap();

        assert!(http.body.get("instructions").is_none());
        assert!(http.body.get("previous_response_id").is_none());
        assert!(http.body.get("max_output_tokens").is_none());
    }

    #[test]
    fn extract_anthropic_pulls_text_tool_calls_and_metadata() {
        let response = json!({
            "id": "msg_1",
            "model": "claude-test",
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "let me check"},
                {"type": "tool_use", "id": "toolu_9", "name": "read", "input": {"path": "a.txt"}},
            ],
        });

        let extracted = extract_turn(ApiKind::AnthropicMessages, &response);

        assert_eq!(extracted.text, "let me check");
        assert_eq!(extracted.finish_reason.as_deref(), Some("tool_use"));
        assert_eq!(extracted.provider_response_id.as_deref(), Some("msg_1"));
        assert_eq!(extracted.provider_model.as_deref(), Some("claude-test"));
        assert_eq!(extracted.tool_calls.len(), 1);
        let tool_call = &extracted.tool_calls[0];
        assert_eq!(tool_call.provider_id, "toolu_9");
        assert_eq!(tool_call.name, "read");
        assert_eq!(tool_call.arguments, json!({"path": "a.txt"}).to_string());
    }

    #[test]
    fn extract_responses_pulls_text_tool_calls_and_metadata() {
        let response = json!({
            "id": "resp_1",
            "model": "gpt-test",
            "status": "completed",
            "output": [
                {"type": "message", "role": "assistant", "content": [
                    {"type": "output_text", "text": "done", "annotations": []}
                ]},
                {"type": "function_call", "call_id": "call_2", "name": "write", "arguments": "{\"path\":\"b.txt\"}"},
            ],
        });

        let extracted = extract_turn(ApiKind::OpenAiResponses, &response);

        assert_eq!(extracted.text, "done");
        assert_eq!(extracted.finish_reason.as_deref(), Some("completed"));
        assert_eq!(extracted.provider_response_id.as_deref(), Some("resp_1"));
        assert_eq!(extracted.tool_calls.len(), 1);
        let tool_call = &extracted.tool_calls[0];
        assert_eq!(tool_call.provider_id, "call_2");
        assert_eq!(tool_call.name, "write");
        assert_eq!(tool_call.arguments, "{\"path\":\"b.txt\"}");
    }
}
