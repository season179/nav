use std::fmt;
use std::time::Duration;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{
    ApiKind, MaxTokensField, ProviderRoutingCompat, ResolveModelError, ResolvedModelConfig,
    ThinkingFormat,
};

#[derive(Debug, Clone, PartialEq)]
pub struct OpenAiCompletionsRequest {
    pub messages: Vec<ChatCompletionRequestMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub stream: bool,
}

impl OpenAiCompletionsRequest {
    pub fn new(messages: Vec<ChatCompletionRequestMessage>) -> Self {
        Self {
            messages,
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
            stream: false,
        }
    }

    pub fn from_user(content: impl Into<String>) -> Self {
        Self::new(vec![ChatCompletionRequestMessage::user(content)])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatCompletionMessageRole {
    System,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCompletionRequestMessage {
    pub role: ChatCompletionMessageRole,
    pub content: String,
}

impl ChatCompletionRequestMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatCompletionMessageRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatCompletionMessageRole::User,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatCompletionRequestPlan {
    pub endpoint: String,
    pub body: Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatCompletionResponse {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub choices: Vec<ChatCompletionChoice>,
    #[serde(default)]
    pub usage: Option<ChatCompletionUsage>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatCompletionChoice {
    #[serde(default)]
    pub index: Option<u32>,
    pub message: ChatCompletionResponseMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatCompletionResponseMessage {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatCompletionStreamChunk {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub choices: Vec<ChatCompletionStreamChoice>,
    #[serde(default)]
    pub usage: Option<ChatCompletionUsage>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatCompletionStreamEvent {
    Chunk(ChatCompletionStreamChunk),
    Done,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatCompletionStreamChoice {
    #[serde(default)]
    pub index: Option<u32>,
    pub delta: ChatCompletionDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatCompletionDelta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChatCompletionUsage {
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
    #[serde(default)]
    pub total_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiCompletionsProviderError {
    pub status: u16,
    pub message: String,
    pub error_type: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAiCompletionsError {
    MissingApiKey {
        provider_id: String,
        env_var: Option<String>,
    },
    UnsupportedApi {
        api: ApiKind,
    },
    InvalidBaseUrl {
        base_url: String,
        message: String,
    },
    Transport {
        message: String,
    },
    StreamingUnsupported,
    Http {
        status: u16,
        body: String,
    },
    Provider(OpenAiCompletionsProviderError),
    ModelResolution {
        error: ResolveModelError,
    },
    MalformedResponse {
        message: String,
    },
}

impl fmt::Display for OpenAiCompletionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey {
                provider_id,
                env_var,
            } => match env_var {
                Some(env_var) => write!(
                    formatter,
                    "missing API key for provider `{provider_id}` from environment variable `{env_var}`"
                ),
                None => write!(formatter, "missing API key for provider `{provider_id}`"),
            },
            Self::UnsupportedApi { api } => {
                write!(
                    formatter,
                    "unsupported model API for chat completions: {api:?}"
                )
            }
            Self::InvalidBaseUrl { base_url, message } => {
                write!(
                    formatter,
                    "invalid provider base URL `{base_url}`: {message}"
                )
            }
            Self::Transport { message } => write!(formatter, "request transport failed: {message}"),
            Self::StreamingUnsupported => write!(
                formatter,
                "streaming chat completions are not supported by complete()"
            ),
            Self::Http { status, body } => {
                write!(formatter, "provider returned HTTP {status}: {body}")
            }
            Self::Provider(error) => write!(
                formatter,
                "provider returned HTTP {}: {}",
                error.status, error.message
            ),
            Self::ModelResolution { error } => {
                write!(formatter, "failed to resolve model config: {error:?}")
            }
            Self::MalformedResponse { message } => {
                write!(formatter, "malformed provider response: {message}")
            }
        }
    }
}

impl std::error::Error for OpenAiCompletionsError {}

impl From<ResolveModelError> for OpenAiCompletionsError {
    fn from(error: ResolveModelError) -> Self {
        match error {
            ResolveModelError::MissingApiKey {
                provider_id,
                env_var,
            } => Self::MissingApiKey {
                provider_id,
                env_var,
            },
            error => Self::ModelResolution { error },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct OpenAiCompletionsResponseParser;

impl OpenAiCompletionsResponseParser {
    pub fn parse_response(
        body: &str,
        api_key: &str,
    ) -> Result<ChatCompletionResponse, OpenAiCompletionsError> {
        let response = serde_json::from_str::<ChatCompletionResponse>(body).map_err(|error| {
            OpenAiCompletionsError::MalformedResponse {
                message: redact_secret(&error.to_string(), api_key),
            }
        })?;

        if response.choices.is_empty() {
            return Err(OpenAiCompletionsError::MalformedResponse {
                message: "response did not include any choices".to_string(),
            });
        }

        Ok(response)
    }

    pub fn parse_stream_chunk(
        body: &str,
        api_key: &str,
    ) -> Result<ChatCompletionStreamChunk, OpenAiCompletionsError> {
        let chunk = serde_json::from_str::<ChatCompletionStreamChunk>(body).map_err(|error| {
            OpenAiCompletionsError::MalformedResponse {
                message: redact_secret(&error.to_string(), api_key),
            }
        })?;

        if chunk.choices.is_empty() && chunk.usage.is_none() {
            return Err(OpenAiCompletionsError::MalformedResponse {
                message: "stream chunk did not include choices or usage".to_string(),
            });
        }

        Ok(chunk)
    }

    pub fn parse_stream_event(
        event: &str,
        api_key: &str,
    ) -> Result<Option<ChatCompletionStreamEvent>, OpenAiCompletionsError> {
        parse_stream_event(event, api_key)
    }

    pub fn parse_error_response(status: u16, body: &str, api_key: &str) -> OpenAiCompletionsError {
        parse_error_response(status, body, api_key)
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompletionsClient {
    http: reqwest::Client,
}

impl Default for OpenAiCompletionsClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiCompletionsClient {
    pub fn new() -> Self {
        Self {
            http: default_http_client(),
        }
    }

    pub fn with_http_client(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub fn build_request(
        &self,
        model: &ResolvedModelConfig,
        request: &OpenAiCompletionsRequest,
    ) -> Result<ChatCompletionRequestPlan, OpenAiCompletionsError> {
        validate_resolved_model(model)?;

        Ok(ChatCompletionRequestPlan {
            endpoint: chat_completions_endpoint(&model.base_url)?,
            body: request_body(model, request),
        })
    }

    pub async fn complete(
        &self,
        model: &ResolvedModelConfig,
        request: &OpenAiCompletionsRequest,
    ) -> Result<ChatCompletionResponse, OpenAiCompletionsError> {
        if request.stream {
            return Err(OpenAiCompletionsError::StreamingUnsupported);
        }

        let plan = self.build_request(model, request)?;
        let api_key = model.api_key.expose_secret();
        let response = self
            .http
            .post(&plan.endpoint)
            .bearer_auth(api_key)
            .json(&plan.body)
            .send()
            .await
            .map_err(|error| OpenAiCompletionsError::Transport {
                message: redact_secret(&error.to_string(), api_key),
            })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| OpenAiCompletionsError::Transport {
                message: redact_secret(&error.to_string(), api_key),
            })?;

        if !status.is_success() {
            return Err(OpenAiCompletionsResponseParser::parse_error_response(
                status.as_u16(),
                &body,
                api_key,
            ));
        }

        OpenAiCompletionsResponseParser::parse_response(&body, api_key)
    }
}

fn validate_resolved_model(model: &ResolvedModelConfig) -> Result<(), OpenAiCompletionsError> {
    if model.api != ApiKind::OpenAiCompletions {
        return Err(OpenAiCompletionsError::UnsupportedApi { api: model.api });
    }

    if model.api_key.expose_secret().trim().is_empty() {
        return Err(OpenAiCompletionsError::MissingApiKey {
            provider_id: model.provider_id.clone(),
            env_var: None,
        });
    }

    Ok(())
}

fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("default reqwest client configuration should be valid")
}

fn chat_completions_endpoint(base_url: &str) -> Result<String, OpenAiCompletionsError> {
    let mut url = Url::parse(base_url).map_err(|error| OpenAiCompletionsError::InvalidBaseUrl {
        base_url: base_url.to_string(),
        message: error.to_string(),
    })?;

    let base_path = url.path().trim_end_matches('/');
    let endpoint_path = if base_path.is_empty() {
        "/chat/completions".to_string()
    } else {
        format!("{base_path}/chat/completions")
    };
    url.set_path(&endpoint_path);
    url.set_query(None);
    url.set_fragment(None);

    Ok(url.to_string())
}

fn request_body(model: &ResolvedModelConfig, request: &OpenAiCompletionsRequest) -> Value {
    let messages = request
        .messages
        .iter()
        .map(|message| message_value(model, message))
        .collect::<Vec<_>>();

    let mut body = Map::new();
    body.insert("model".to_string(), json!(model.model.id));
    body.insert("messages".to_string(), Value::Array(messages));
    body.insert("stream".to_string(), json!(request.stream));

    if request.stream && model.compat.supports_usage_in_streaming == Some(true) {
        body.insert(
            "stream_options".to_string(),
            json!({
                "include_usage": true,
            }),
        );
    }

    if let Some(max_tokens) = request.max_tokens {
        let field_name = match model
            .compat
            .max_tokens_field
            .unwrap_or(MaxTokensField::MaxCompletionTokens)
        {
            MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
            MaxTokensField::MaxTokens => "max_tokens",
        };
        body.insert(field_name.to_string(), json!(max_tokens));
    }

    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }

    if model.model.reasoning {
        apply_reasoning_settings(model, request, &mut body);
    }

    if let Some(routing) = model.compat.routing.as_ref().and_then(routing_value) {
        body.insert("provider".to_string(), routing);
    }

    Value::Object(body)
}

fn message_value(model: &ResolvedModelConfig, message: &ChatCompletionRequestMessage) -> Value {
    let role = match message.role {
        ChatCompletionMessageRole::System => {
            if model.compat.supports_developer_role == Some(true) {
                "developer"
            } else {
                "system"
            }
        }
        ChatCompletionMessageRole::User => "user",
    };

    json!({
        "role": role,
        "content": message.content,
    })
}

fn apply_reasoning_settings(
    model: &ResolvedModelConfig,
    request: &OpenAiCompletionsRequest,
    body: &mut Map<String, Value>,
) {
    let Some(reasoning_effort) = request.reasoning_effort else {
        if matches!(
            model.compat.thinking_format,
            Some(ThinkingFormat::Qwen)
                | Some(ThinkingFormat::QwenChatTemplate)
                | Some(ThinkingFormat::Zai)
        ) {
            apply_thinking_enabled(model, body, false);
        }
        return;
    };

    let effort = json!(reasoning_effort);
    match model
        .compat
        .thinking_format
        .unwrap_or(ThinkingFormat::OpenAi)
    {
        ThinkingFormat::OpenAi if model.compat.supports_reasoning_effort == Some(true) => {
            body.insert("reasoning_effort".to_string(), effort);
        }
        ThinkingFormat::OpenRouter => {
            body.insert(
                "reasoning".to_string(),
                json!({ "effort": reasoning_effort }),
            );
        }
        ThinkingFormat::DeepSeek => {
            body.insert("thinking".to_string(), json!({ "type": "enabled" }));
            if model.compat.supports_reasoning_effort == Some(true) {
                body.insert("reasoning_effort".to_string(), effort);
            }
        }
        ThinkingFormat::Together => {
            body.insert("reasoning".to_string(), json!({ "enabled": true }));
            if model.compat.supports_reasoning_effort == Some(true) {
                body.insert("reasoning_effort".to_string(), effort);
            }
        }
        ThinkingFormat::Zai | ThinkingFormat::Qwen | ThinkingFormat::QwenChatTemplate => {
            apply_thinking_enabled(model, body, true);
        }
        ThinkingFormat::OpenAi => {}
    }
}

fn apply_thinking_enabled(
    model: &ResolvedModelConfig,
    body: &mut Map<String, Value>,
    enabled: bool,
) {
    match model
        .compat
        .thinking_format
        .unwrap_or(ThinkingFormat::OpenAi)
    {
        ThinkingFormat::QwenChatTemplate => {
            body.insert(
                "chat_template_kwargs".to_string(),
                json!({
                    "enable_thinking": enabled,
                    "preserve_thinking": true,
                }),
            );
        }
        ThinkingFormat::Zai | ThinkingFormat::Qwen => {
            body.insert("enable_thinking".to_string(), json!(enabled));
        }
        _ => {}
    }
}

fn routing_value(routing: &ProviderRoutingCompat) -> Option<Value> {
    let mut value = Map::new();

    if let Some(allow_fallbacks) = routing.allow_fallbacks {
        value.insert("allow_fallbacks".to_string(), json!(allow_fallbacks));
    }
    if let Some(require_parameters) = routing.require_parameters {
        value.insert("require_parameters".to_string(), json!(require_parameters));
    }
    if let Some(only) = &routing.only {
        value.insert("only".to_string(), json!(only));
    }
    if let Some(order) = &routing.order {
        value.insert("order".to_string(), json!(order));
    }
    if let Some(ignore) = &routing.ignore {
        value.insert("ignore".to_string(), json!(ignore));
    }

    (!value.is_empty()).then_some(Value::Object(value))
}

fn parse_error_response(status: u16, body: &str, api_key: &str) -> OpenAiCompletionsError {
    let redacted_body = redact_secret(body, api_key);

    if let Ok(value) = serde_json::from_str::<Value>(&redacted_body)
        && let Some(error) = provider_error_from_value(status, &value)
    {
        return OpenAiCompletionsError::Provider(error);
    }

    OpenAiCompletionsError::Http {
        status,
        body: redacted_body,
    }
}

fn parse_stream_event(
    event: &str,
    api_key: &str,
) -> Result<Option<ChatCompletionStreamEvent>, OpenAiCompletionsError> {
    let Some(data) = stream_event_data(event) else {
        return Ok(None);
    };

    if data == "[DONE]" {
        return Ok(Some(ChatCompletionStreamEvent::Done));
    }

    OpenAiCompletionsResponseParser::parse_stream_chunk(&data, api_key)
        .map(ChatCompletionStreamEvent::Chunk)
        .map(Some)
}

fn stream_event_data(event: &str) -> Option<String> {
    let mut data_lines = Vec::new();

    for line in event.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }

        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        data_lines.push(data.strip_prefix(' ').unwrap_or(data));
    }

    if data_lines.is_empty() {
        let trimmed = event.trim();
        (trimmed == "[DONE]" || trimmed.starts_with('{')).then(|| trimmed.to_string())
    } else {
        Some(data_lines.join("\n"))
    }
}

fn provider_error_from_value(status: u16, value: &Value) -> Option<OpenAiCompletionsProviderError> {
    let error = value.get("error")?;
    match error {
        Value::String(message) if !message.is_empty() => Some(OpenAiCompletionsProviderError {
            status,
            message: message.clone(),
            error_type: None,
            code: None,
        }),
        Value::Object(error) => {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.get("error").and_then(Value::as_str))?
                .to_string();
            if message.is_empty() {
                return None;
            }

            Some(OpenAiCompletionsProviderError {
                status,
                message,
                error_type: error
                    .get("type")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                code: error.get("code").and_then(value_to_error_code),
            })
        }
        _ => None,
    }
}

fn value_to_error_code(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn redact_secret(value: &str, secret: &str) -> String {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        value.to_string()
    } else {
        value.replace(trimmed, "<redacted>")
    }
}
