use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use crate::events::{
    HarnessEventEnvelope, HarnessEventIdSource, ModelOutputContext, OpenAiStreamEventMapper,
};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::Notify;

use crate::sessions::{ModelTurn, ModelTurnRole, ToolCall};
use crate::tools::{NavTool, ToolPreset, ToolRegistry};

use super::{
    ApiKind, ContextLimitError, MaxTokensField, ProviderRoutingCompat, ResolveModelError,
    ResolvedModelConfig, ThinkingFormat, classify_context_limit, classify_streamed_context_limit,
};

const OPENAI_COMPLETIONS_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct OpenAiCompletionsRequest {
    pub messages: Vec<ChatCompletionRequestMessage>,
    pub tools: Vec<ChatCompletionToolDefinition>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub stream: bool,
    pub prompt_cache_key: Option<String>,
    pub prompt_cache_retention: Option<String>,
}

impl OpenAiCompletionsRequest {
    pub fn new(messages: Vec<ChatCompletionRequestMessage>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            reasoning_effort: None,
            stream: false,
            prompt_cache_key: None,
            prompt_cache_retention: Some("in_memory".to_string()),
        }
    }

    pub fn from_user(content: impl Into<String>) -> Self {
        Self::new(vec![ChatCompletionRequestMessage::user(content)])
    }

    pub fn from_turns(turns: &[ModelTurn]) -> Self {
        Self::new(
            turns
                .iter()
                .map(ChatCompletionRequestMessage::from_turn)
                .collect(),
        )
    }

    pub fn from_turns_with_tools(
        turns: &[ModelTurn],
        registry: &ToolRegistry,
        preset: ToolPreset,
    ) -> Self {
        let mut request = Self::from_turns(turns);
        request.tools = registry
            .preset_tools(preset)
            .into_iter()
            .map(|tool| ChatCompletionToolDefinition::from_tool(tool.as_ref()))
            .collect();
        request
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatCompletionToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ChatCompletionToolDefinition {
    pub(crate) fn from_tool(tool: &dyn NavTool) -> Self {
        Self {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            parameters: tool.parameters(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatCompletionMessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool call inside an assistant message.
///
/// Follows the OpenAI schema:
/// `{ "id": "call_...", "type": "function", "function": { "name": "...", "arguments": "..." } }`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCompletionToolCall {
    pub id: String,
    pub function: ChatCompletionToolCallFunction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCompletionToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// A single message in a chat completions request.
///
/// ## Serialization shape
///
/// | Role      | Fields                                  |
/// |-----------|-----------------------------------------|
/// | `system`  | `role`, `content`                       |
/// | `user`    | `role`, `content`                       |
/// | `assistant`| `role`, `content` (may be null), `tool_calls` (optional) |
/// | `tool`    | `role`, `tool_call_id`, `content`       |
///
/// `tool_calls` is an array of `{ id, type: "function", function: { name, arguments } }`.
/// A tool-role message carries the result for one `tool_call_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCompletionRequestMessage {
    pub role: ChatCompletionMessageRole,
    pub content: Option<Value>,
    pub tool_calls: Option<Vec<ChatCompletionToolCall>>,
    pub tool_call_id: Option<String>,
}

impl ChatCompletionRequestMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatCompletionMessageRole::System,
            content: Some(json!(content.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatCompletionMessageRole::User,
            content: Some(json!(content.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatCompletionMessageRole::Assistant,
            content: Some(json!(content.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_with_tool_calls(tool_calls: Vec<ChatCompletionToolCall>) -> Self {
        Self {
            role: ChatCompletionMessageRole::Assistant,
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn assistant_with_content_and_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ChatCompletionToolCall>,
    ) -> Self {
        Self {
            role: ChatCompletionMessageRole::Assistant,
            content: Some(json!(content.into())),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatCompletionMessageRole::Tool,
            content: Some(json!(content.into())),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    fn from_turn(turn: &ModelTurn) -> Self {
        match turn.role {
            ModelTurnRole::System => Self::system(turn.text_content()),
            ModelTurnRole::User => Self::user(turn.text_content()),
            ModelTurnRole::Assistant => {
                let tool_calls = turn
                    .tool_calls()
                    .into_iter()
                    .map(chat_completion_tool_call)
                    .collect::<Vec<_>>();
                let content = turn.text_content();
                if tool_calls.is_empty() {
                    Self::assistant(content)
                } else if content.is_empty() {
                    Self::assistant_with_tool_calls(tool_calls)
                } else {
                    Self::assistant_with_content_and_tool_calls(content, tool_calls)
                }
            }
            ModelTurnRole::Tool => {
                Self::tool(turn.tool_call_id().unwrap_or_default(), turn.text_content())
            }
        }
    }
}

fn chat_completion_tool_call(tool_call: ToolCall) -> ChatCompletionToolCall {
    ChatCompletionToolCall {
        id: tool_call.id,
        function: ChatCompletionToolCallFunction {
            name: tool_call.name,
            arguments: tool_call.arguments,
        },
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
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub reasoning_text: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ChatCompletionToolCallDelta>,
}

impl ChatCompletionDelta {
    pub fn reasoning_delta(&self) -> Option<&str> {
        [
            self.reasoning_content.as_deref(),
            self.reasoning.as_deref(),
            self.reasoning_text.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|delta| !delta.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatCompletionToolCallDelta {
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, rename = "type")]
    pub tool_type: Option<String>,
    #[serde(default)]
    pub function: Option<ChatCompletionToolCallFunctionDelta>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatCompletionToolCallFunctionDelta {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
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
pub struct OpenAiCompletionsStreamProviderError {
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
    Cancelled,
    ContextLimit(ContextLimitError),
    Provider(OpenAiCompletionsProviderError),
    ProviderStream(OpenAiCompletionsStreamProviderError),
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
            Self::Cancelled => write!(formatter, "request cancelled"),
            Self::ContextLimit(error) => write!(
                formatter,
                "provider returned HTTP {} (context limit exceeded): {}",
                error.status, error.message
            ),
            Self::Provider(error) => write!(
                formatter,
                "provider returned HTTP {}: {}",
                error.status, error.message
            ),
            Self::ProviderStream(error) => {
                write!(
                    formatter,
                    "provider stream returned error: {}",
                    error.message
                )
            }
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

#[derive(Debug, Clone, Default)]
pub struct OpenAiCompletionsRequestContext {
    cancellation_token: Option<OpenAiCompletionsCancellationToken>,
}

impl OpenAiCompletionsRequestContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cancellation_token(
        mut self,
        cancellation_token: OpenAiCompletionsCancellationToken,
    ) -> Self {
        self.cancellation_token = Some(cancellation_token);
        self
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation_token
            .as_ref()
            .is_some_and(OpenAiCompletionsCancellationToken::is_cancelled)
    }

    async fn cancelled(&self) {
        match &self.cancellation_token {
            Some(cancellation_token) => cancellation_token.cancelled().await,
            None => std::future::pending().await,
        }
    }
}

#[derive(Debug)]
struct OpenAiCompletionsCancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompletionsCancellationToken {
    state: Arc<OpenAiCompletionsCancellationState>,
}

impl Default for OpenAiCompletionsCancellationToken {
    fn default() -> Self {
        Self {
            state: Arc::new(OpenAiCompletionsCancellationState {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }
}

impl OpenAiCompletionsCancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.state.cancelled.swap(true, Ordering::SeqCst) {
            self.state.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.state.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if self.is_cancelled() {
                return;
            }

            notified.await;
        }
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
            .timeout(OPENAI_COMPLETIONS_REQUEST_TIMEOUT)
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

    pub async fn stream_events(
        &self,
        model: &ResolvedModelConfig,
        request: &OpenAiCompletionsRequest,
        output_context: ModelOutputContext,
        ids: &mut impl HarnessEventIdSource,
        emit: impl FnMut(Vec<HarnessEventEnvelope>),
    ) -> Result<(), OpenAiCompletionsError> {
        let request_context = OpenAiCompletionsRequestContext::default();
        self.stream_events_with_context(model, request, &request_context, output_context, ids, emit)
            .await
    }

    pub async fn stream_events_with_context(
        &self,
        model: &ResolvedModelConfig,
        request: &OpenAiCompletionsRequest,
        request_context: &OpenAiCompletionsRequestContext,
        output_context: ModelOutputContext,
        ids: &mut impl HarnessEventIdSource,
        mut emit: impl FnMut(Vec<HarnessEventEnvelope>),
    ) -> Result<(), OpenAiCompletionsError> {
        if request_context.is_cancelled() {
            return Err(OpenAiCompletionsError::Cancelled);
        }

        let mut streaming_request = request.clone();
        streaming_request.stream = true;

        let plan = self.build_request(model, &streaming_request)?;
        let api_key = model.api_key.expose_secret();
        let mut response = self
            .http
            .post(&plan.endpoint)
            .bearer_auth(api_key)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&plan.body)
            .send()
            .await
            .map_err(|error| OpenAiCompletionsError::Transport {
                message: redact_secret(&error.to_string(), api_key),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body =
                response
                    .text()
                    .await
                    .map_err(|error| OpenAiCompletionsError::Transport {
                        message: redact_secret(&error.to_string(), api_key),
                    })?;
            return Err(OpenAiCompletionsResponseParser::parse_error_response(
                status.as_u16(),
                &body,
                api_key,
            ));
        }

        let mut mapper = OpenAiStreamEventMapper::new(output_context);
        let mut buffer = SseEventBuffer::default();

        loop {
            if request_context.is_cancelled() {
                return Err(OpenAiCompletionsError::Cancelled);
            }

            let chunk = tokio::select! {
                _ = request_context.cancelled() => return Err(OpenAiCompletionsError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| OpenAiCompletionsError::Transport {
                    message: redact_secret(&error.to_string(), api_key),
                })?,
            };

            let Some(chunk) = chunk else {
                break;
            };

            let raw_events = match buffer.push_chunk(&chunk, api_key) {
                Ok(raw_events) => raw_events,
                Err(error) => {
                    return emit_stream_error(error, &mut mapper, ids, &mut emit);
                }
            };

            if handle_raw_stream_events(raw_events, api_key, &mut mapper, ids, &mut emit)? {
                return Ok(());
            }
        }

        let final_raw_event = match buffer.finish(api_key) {
            Ok(raw_event) => raw_event,
            Err(error) => {
                return emit_stream_error(error, &mut mapper, ids, &mut emit);
            }
        };
        if let Some(raw_event) = final_raw_event
            && handle_raw_stream_events(vec![raw_event], api_key, &mut mapper, ids, &mut emit)?
        {
            return Ok(());
        }

        emit_stream_error(
            OpenAiCompletionsError::MalformedResponse {
                message: "stream ended before [DONE]".to_string(),
            },
            &mut mapper,
            ids,
            &mut emit,
        )
    }
}

fn handle_raw_stream_events(
    raw_events: Vec<String>,
    api_key: &str,
    mapper: &mut OpenAiStreamEventMapper,
    ids: &mut impl HarnessEventIdSource,
    emit: &mut impl FnMut(Vec<HarnessEventEnvelope>),
) -> Result<bool, OpenAiCompletionsError> {
    for raw_event in raw_events {
        let result = OpenAiCompletionsResponseParser::parse_stream_event(&raw_event, api_key);
        let error = result.as_ref().err().cloned();
        if emit_mapped_stream_result(result, mapper, ids, emit) {
            return match error {
                Some(error) => Err(error),
                None => Ok(true),
            };
        }
    }

    Ok(false)
}

fn emit_stream_error(
    error: OpenAiCompletionsError,
    mapper: &mut OpenAiStreamEventMapper,
    ids: &mut impl HarnessEventIdSource,
    emit: &mut impl FnMut(Vec<HarnessEventEnvelope>),
) -> Result<(), OpenAiCompletionsError> {
    emit_mapped_stream_result(Err(error.clone()), mapper, ids, emit);
    Err(error)
}

fn emit_mapped_stream_result(
    result: Result<Option<ChatCompletionStreamEvent>, OpenAiCompletionsError>,
    mapper: &mut OpenAiStreamEventMapper,
    ids: &mut impl HarnessEventIdSource,
    emit: &mut impl FnMut(Vec<HarnessEventEnvelope>),
) -> bool {
    let is_terminal = matches!(result, Ok(Some(ChatCompletionStreamEvent::Done)) | Err(_));
    let events = mapper.map_stream_result(result, ids);
    if !events.is_empty() {
        emit(events);
    }
    is_terminal
}

#[derive(Debug, Default)]
struct SseEventBuffer {
    bytes: Vec<u8>,
}

impl SseEventBuffer {
    fn push_chunk(
        &mut self,
        chunk: &[u8],
        api_key: &str,
    ) -> Result<Vec<String>, OpenAiCompletionsError> {
        self.bytes.extend_from_slice(chunk);
        let mut events = Vec::new();

        while let Some((event_end, delimiter_len)) = find_sse_event_boundary(&self.bytes) {
            let event_bytes = self.bytes[..event_end].to_vec();
            self.bytes.drain(..event_end + delimiter_len);
            events.push(decode_sse_event(event_bytes, api_key)?);
        }

        Ok(events)
    }

    fn finish(&mut self, api_key: &str) -> Result<Option<String>, OpenAiCompletionsError> {
        if self.bytes.iter().all(u8::is_ascii_whitespace) {
            self.bytes.clear();
            return Ok(None);
        }

        let event_bytes = std::mem::take(&mut self.bytes);
        decode_sse_event(event_bytes, api_key).map(Some)
    }
}

fn find_sse_event_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    [
        b"\r\n\r\n".as_slice(),
        b"\n\n".as_slice(),
        b"\r\r".as_slice(),
    ]
    .into_iter()
    .filter_map(|delimiter| find_bytes(bytes, delimiter).map(|index| (index, delimiter.len())))
    .min_by_key(|(index, _)| *index)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn decode_sse_event(bytes: Vec<u8>, api_key: &str) -> Result<String, OpenAiCompletionsError> {
    String::from_utf8(bytes).map_err(|error| OpenAiCompletionsError::MalformedResponse {
        message: redact_secret(&error.to_string(), api_key),
    })
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
        .connect_timeout(OPENAI_COMPLETIONS_REQUEST_TIMEOUT)
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

    if !request.tools.is_empty() {
        body.insert(
            "tools".to_string(),
            json!(
                request
                    .tools
                    .iter()
                    .map(tool_definition_value)
                    .collect::<Vec<_>>()
            ),
        );
    }

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

    if let Some(key) = &request.prompt_cache_key {
        body.insert("prompt_cache_key".to_string(), json!(key));
    }

    if let Some(retention) = &request.prompt_cache_retention {
        body.insert("prompt_cache_retention".to_string(), json!(retention));
    }

    Value::Object(body)
}

fn tool_definition_value(tool: &ChatCompletionToolDefinition) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        },
    })
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
        ChatCompletionMessageRole::Assistant => "assistant",
        ChatCompletionMessageRole::Tool => "tool",
    };

    let content = message.content.clone().unwrap_or(Value::Null);

    let mut obj = json!({
        "role": role,
        "content": content,
    });

    let obj_map = obj.as_object_mut().expect("json! produces an object");

    if let Some(tool_call_id) = &message.tool_call_id {
        obj_map.insert("tool_call_id".to_string(), json!(tool_call_id));
    }

    if let Some(tool_calls) = &message.tool_calls {
        obj_map.insert(
            "tool_calls".to_string(),
            json!(
                tool_calls
                    .iter()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.function.name,
                                "arguments": tc.function.arguments,
                            },
                        })
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }

    obj
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

    if let Some(context_limit) =
        classify_context_limit(ApiKind::OpenAiCompletions, status, &redacted_body)
    {
        return OpenAiCompletionsError::ContextLimit(context_limit);
    }

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

    let redacted_data = redact_secret(&data, api_key);
    if let Some(context_limit) =
        classify_streamed_context_limit(ApiKind::OpenAiCompletions, &redacted_data)
    {
        return Err(OpenAiCompletionsError::ContextLimit(context_limit));
    }
    if let Ok(value) = serde_json::from_str::<Value>(&redacted_data)
        && let Some(error) = provider_stream_error_from_value(&value)
    {
        return Err(OpenAiCompletionsError::ProviderStream(error));
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
    let (message, error_type, code) = provider_error_fields_from_value(value)?;
    Some(OpenAiCompletionsProviderError {
        status,
        message,
        error_type,
        code,
    })
}

fn provider_stream_error_from_value(value: &Value) -> Option<OpenAiCompletionsStreamProviderError> {
    let (message, error_type, code) = provider_error_fields_from_value(value)?;
    Some(OpenAiCompletionsStreamProviderError {
        message,
        error_type,
        code,
    })
}

fn provider_error_fields_from_value(
    value: &Value,
) -> Option<(String, Option<String>, Option<String>)> {
    let error = value.get("error")?;
    match error {
        Value::String(message) if !message.is_empty() => Some((message.clone(), None, None)),
        Value::Object(error) => {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.get("error").and_then(Value::as_str))?
                .to_string();
            if message.is_empty() {
                return None;
            }

            Some((
                message,
                error
                    .get("type")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                error.get("code").and_then(value_to_error_code),
            ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ApiKeyConfig, ModelConfig, ProviderCompat, ProviderConfig, resolver::ResolvedApiKey};

    fn resolved_model() -> ResolvedModelConfig {
        ResolvedModelConfig {
            compat: ProviderCompat::default(),
            api: ApiKind::OpenAiCompletions,
            base_url: "https://api.openai.com/v1".to_string(),
            provider_id: "openai".to_string(),
            provider: ProviderConfig {
                name: None,
                api: ApiKind::OpenAiCompletions,
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: ApiKeyConfig::Inline {
                    inline: "sk-test".to_string(),
                },
                models: Vec::new(),
                compat: ProviderCompat::default(),
            },
            model: ModelConfig {
                id: "gpt-4o".to_string(),
                name: None,
                api: None,
                base_url: None,
                reasoning: false,
                input: Vec::new(),
                context_window: None,
                max_tokens: None,
                compat: ProviderCompat::default(),
            },
            api_key: ResolvedApiKey::new("sk-test"),
        }
    }

    fn request_with_cache_fields(
        cache_key: Option<&str>,
        retention: Option<&str>,
    ) -> OpenAiCompletionsRequest {
        let mut request = OpenAiCompletionsRequest::from_user("hello");
        if let Some(key) = cache_key {
            request.prompt_cache_key = Some(key.to_owned());
        }
        if let Some(ret) = retention {
            request.prompt_cache_retention = Some(ret.to_owned());
        }
        request
    }

    #[test]
    fn body_contains_prompt_cache_key_when_set() {
        let model = resolved_model();
        let request = request_with_cache_fields(Some("session-123"), None);
        let body = request_body(&model, &request);

        assert_eq!(body["prompt_cache_key"], json!("session-123"));
    }

    #[test]
    fn body_contains_prompt_cache_retention_when_set() {
        let model = resolved_model();
        let request = request_with_cache_fields(None, Some("24h"));
        let body = request_body(&model, &request);

        assert_eq!(body["prompt_cache_retention"], json!("24h"));
    }

    #[test]
    fn body_omits_cache_key_when_not_set() {
        let model = resolved_model();
        let request = request_with_cache_fields(None, None);
        let body = request_body(&model, &request);

        assert!(body.get("prompt_cache_key").is_none());
        assert_eq!(body["prompt_cache_retention"], json!("in_memory"));
    }

    #[test]
    fn body_contains_both_cache_fields() {
        let model = resolved_model();
        let request = request_with_cache_fields(Some("session-abc"), Some("in_memory"));
        let body = request_body(&model, &request);

        assert_eq!(body["prompt_cache_key"], json!("session-abc"));
        assert_eq!(body["prompt_cache_retention"], json!("in_memory"));
    }
}
