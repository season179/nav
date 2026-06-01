//! Text-model abstraction for the chat/agent loop.
//!
//! A [`ChatModel`] turns assembled Model Context (plus the tools it may call)
//! into one assistant turn: free text, one or more tool calls, or both. Two
//! implementations share this interface: [`MockModel`] is deterministic and
//! used by tests and offline UI smoke, while the real OpenAI-compatible client
//! talks to a configured provider.

use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{ConfigError, ResolvedModelConfig};
use crate::context::ModelContext;
use crate::tokens::{
    HeuristicTokenCounter, TextTokenCounter, TokenEstimate, TokenUsage, counter_from_compat,
    estimate_assistant_output, estimate_model_context,
};

const DEFAULT_OPENAI_MODEL: &str = "gpt-4o-mini";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Message shown when no model can be resolved from settings or the environment.
const NOT_CONFIGURED_MESSAGE: &str = "model not configured: add a default model to \
     ~/.nav/settings.json, set NAV_API_KEY (and optionally NAV_MODEL/NAV_BASE_URL) for an \
     OpenAI-compatible provider, or NAV_MOCK_MODEL=1 for the deterministic mock";

/// Who authored a chat message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// A tool result fed back to the model after executing a tool call.
    Tool,
}

impl Role {
    /// Wire name used in events and provider requests.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// One tool call requested by an assistant turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    /// Provider-assigned id; tool results refer back to it.
    pub id: String,
    /// Tool name the model wants to invoke.
    pub name: String,
    /// Raw JSON arguments string, exactly as the provider emitted them.
    pub arguments: String,
}

/// One message-shaped entry shared by Turn History and Model Context.
///
/// Plain user and assistant turns carry only `content`. An assistant turn may
/// additionally carry `tool_calls`; a [`Role::Tool`] turn carries the
/// `tool_call_id` it answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Provider reasoning/thinking payload for assistant turns. Some
    /// OpenAI-compatible thinking models require this to be replayed verbatim.
    pub reasoning_content: Option<String>,
    /// Tool calls requested by an assistant turn (empty for every other turn).
    pub tool_calls: Vec<ToolCall>,
    /// For a [`Role::Tool`] turn, the assistant tool call this result answers.
    pub tool_call_id: Option<String>,
    /// For a [`Role::Tool`] turn, whether the tool failed. Always `false` for
    /// other turns. Not sent to the model — it only lets a resumed session
    /// replay a failed tool with the same styling it had live.
    pub is_error: bool,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            is_error: false,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            is_error: false,
        }
    }

    /// Attach provider reasoning/thinking payload to an assistant turn.
    pub fn with_reasoning_content(mut self, reasoning_content: impl Into<String>) -> Self {
        self.reasoning_content = Some(reasoning_content.into());
        self
    }

    /// An assistant turn that requests one or more tool calls. `content` may be
    /// empty when the model returned only tool calls.
    pub fn assistant_tool_calls(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            reasoning_content: None,
            tool_calls,
            tool_call_id: None,
            is_error: false,
        }
    }

    /// An assistant tool-call turn that carries provider reasoning/thinking
    /// payload to replay on later model calls.
    pub fn assistant_tool_calls_with_reasoning(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
        reasoning_content: impl Into<String>,
    ) -> Self {
        Self::assistant_tool_calls(content, tool_calls).with_reasoning_content(reasoning_content)
    }

    /// A tool result answering a specific assistant tool call. `is_error` marks
    /// a failed tool run (an unknown tool, bad arguments, or a tool error).
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            is_error,
        }
    }
}

/// A tool advertised to the model in a request.
#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub parameters: Value,
}

/// Why the model stopped producing output for a turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other(String),
}

/// One assistant turn produced by the model: text, tool calls, or both.
#[derive(Clone, Debug)]
pub struct ModelResponse {
    /// Assistant text, if any. `None` when the turn is purely tool calls.
    pub content: Option<String>,
    /// Provider reasoning/thinking payload, if returned separately from text.
    pub reasoning_content: Option<String>,
    /// Tool calls the model wants executed before the next turn.
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    /// Token counts reported by the provider, when present. If this is `None`,
    /// the agent loop records an explicit local estimate instead.
    pub token_usage: Option<TokenUsage>,
}

impl ModelResponse {
    /// A plain text reply that requests no tools.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: Some(content.into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            token_usage: None,
        }
    }
}

/// Why a model call failed. Surfaced to the renderer as a `run.failed` event.
#[derive(Debug)]
pub struct ModelError {
    pub message: String,
}

impl ModelError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}

/// A model that produces one assistant turn from Model Context and the available
/// tools. Returning [`ModelResponse::tool_calls`] asks the caller to execute
/// those tools and continue the conversation with their results.
pub trait ChatModel: Send + Sync {
    fn respond(
        &self,
        context: &ModelContext,
        tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError>;

    /// Estimate the tokens the next request will send. Future compaction can
    /// use this before a model call, independent of whether the provider later
    /// reports usage.
    fn estimate_context_tokens(&self, context: &ModelContext, tools: &[ToolDef]) -> TokenEstimate {
        estimate_model_context(context, tools, &HeuristicTokenCounter)
    }

    /// Estimate assistant output when the provider omits usage.
    fn estimate_output_tokens(&self, response: &ModelResponse) -> TokenEstimate {
        estimate_assistant_output(
            response.content.as_deref(),
            response.reasoning_content.as_deref(),
            &response.tool_calls,
            &HeuristicTokenCounter,
        )
    }
}

/// Which text model the backend should use.
pub enum ModelChoice {
    /// Deterministic mock, requested explicitly for tests and offline smoke.
    Mock,
    /// A configured OpenAI-compatible provider.
    OpenAi(OpenAiConfig),
    /// No model configured; sending a message yields a clear failure.
    NotConfigured,
    /// Settings resolved to a config the backend cannot use (e.g. unsupported
    /// API or a missing provider). Sending a message fails with this reason.
    Unavailable(String),
}

/// Small, renderer-facing summary of the active model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip)]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenBudgetInfo>,
}

/// Current context usage against the active model's window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBudgetInfo {
    pub used: u64,
    pub context_window: u64,
}

impl ModelInfo {
    pub fn with_used_tokens(&self, used: Option<u64>) -> Self {
        let mut info = self.clone();
        info.token_usage = self.context_window.map(|context_window| TokenBudgetInfo {
            used: used.unwrap_or(0),
            context_window,
        });
        info
    }
}

impl ModelChoice {
    /// Resolve the backend's model, preferring the Pi-style settings file.
    ///
    /// Resolution order:
    /// 1. Explicit `NAV_MOCK_MODEL` wins so tests and offline smoke never reach
    ///    a real provider.
    /// 2. A resolvable `~/.nav/settings.json` default model selects the real
    ///    OpenAI-compatible provider.
    /// 3. If no settings file exists, fall back to environment configuration so
    ///    the bare `NAV_API_KEY` path keeps working.
    /// 4. A present-but-unusable settings file surfaces its specific error.
    ///
    /// `load_config` is injected (rather than calling [`crate::resolve_default_config`]
    /// directly) so this stays unit-testable without touching the filesystem.
    pub fn resolve<F, L>(get: F, load_config: L) -> Self
    where
        F: Fn(&str) -> Option<String>,
        L: FnOnce() -> Result<ResolvedModelConfig, ConfigError>,
    {
        if get("NAV_MOCK_MODEL").is_some_and(|value| !value.is_empty()) {
            return ModelChoice::Mock;
        }

        match load_config() {
            Ok(config) => ModelChoice::OpenAi(OpenAiConfig::from(config)),
            Err(ConfigError::FileNotFound(_) | ConfigError::HomeDirUnavailable) => {
                ModelChoice::from_env(get)
            }
            Err(error) => ModelChoice::Unavailable(error.to_string()),
        }
    }

    /// Resolve a model from environment lookups only.
    ///
    /// Explicit `NAV_MOCK_MODEL` wins so tests and offline smoke never reach a
    /// real provider. Otherwise a present `NAV_API_KEY` selects the OpenAI
    /// path; with neither, the backend stays unconfigured.
    pub fn from_env<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        if get("NAV_MOCK_MODEL").is_some_and(|value| !value.is_empty()) {
            return ModelChoice::Mock;
        }

        match get("NAV_API_KEY") {
            Some(api_key) if !api_key.is_empty() => {
                let model = non_empty(get("NAV_MODEL"), DEFAULT_OPENAI_MODEL);
                ModelChoice::OpenAi(OpenAiConfig {
                    api_key,
                    base_url: non_empty(get("NAV_BASE_URL"), DEFAULT_OPENAI_BASE_URL),
                    // No display name over env config, so the id is the label.
                    name: model.clone(),
                    model,
                    reasoning: false,
                    thinking_level: "off".to_owned(),
                    context_window: None,
                    compat: None,
                    thinking_level_map: None,
                })
            }
            _ => ModelChoice::NotConfigured,
        }
    }

    /// The active model's identifier, when a real provider is configured. Used
    /// to tag persisted assistant turns; `None` for the mock or no model.
    pub fn model_id(&self) -> Option<String> {
        match self {
            ModelChoice::OpenAi(config) => Some(config.model.clone()),
            _ => None,
        }
    }

    /// A short human-readable label for the backend status line.
    pub fn describe(&self) -> String {
        match self {
            ModelChoice::Mock => "mock model".to_owned(),
            ModelChoice::OpenAi(config) => format!("OpenAI-compatible model {}", config.model),
            ModelChoice::NotConfigured => "model not configured".to_owned(),
            ModelChoice::Unavailable(reason) => format!("model unavailable: {reason}"),
        }
    }

    /// A concise summary for the app's model indicator row.
    pub fn info(&self) -> ModelInfo {
        ModelInfo {
            label: self.label(),
            thinking: self.thinking_level(),
            context_window: self.context_window(),
            token_usage: None,
        }
    }

    /// A concise, human-friendly model name for the app's model indicator.
    /// Unlike [`describe`](Self::describe), this drops protocol jargon so the
    /// UI can show just the model the user configured.
    fn label(&self) -> String {
        match self {
            ModelChoice::Mock => "Mock model".to_owned(),
            ModelChoice::OpenAi(config) => config.name.clone(),
            ModelChoice::NotConfigured => "No model configured".to_owned(),
            ModelChoice::Unavailable(_) => "Model unavailable".to_owned(),
        }
    }

    /// Optional reasoning/thinking level for the app's model metadata row.
    fn thinking_level(&self) -> Option<String> {
        match self {
            ModelChoice::OpenAi(config) => Some(config.thinking_level.clone()),
            _ => None,
        }
    }

    fn context_window(&self) -> Option<u64> {
        match self {
            ModelChoice::OpenAi(config) => config.context_window,
            _ => None,
        }
    }

    /// Build the concrete model behind a shared trait object.
    pub fn into_model(self) -> Arc<dyn ChatModel> {
        match self {
            ModelChoice::Mock => Arc::new(MockModel::new()),
            ModelChoice::OpenAi(config) => Arc::new(OpenAiModel::new(config)),
            ModelChoice::NotConfigured => Arc::new(FailingModel::new(NOT_CONFIGURED_MESSAGE)),
            ModelChoice::Unavailable(reason) => Arc::new(FailingModel::new(reason)),
        }
    }
}

impl fmt::Debug for ModelChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the API key.
        f.write_str(&self.describe())
    }
}

fn non_empty(value: Option<String>, fallback: &str) -> String {
    match value {
        Some(value) if !value.is_empty() => value,
        _ => fallback.to_owned(),
    }
}

/// Connection settings for an OpenAI-compatible chat-completions provider.
pub struct OpenAiConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    /// Human-friendly display name for the model (from settings.json `name`,
    /// falling back to the model id). Shown in the app's model indicator.
    pub name: String,
    /// Whether the model is marked as reasoning-capable in settings.json.
    pub reasoning: bool,
    /// The resolved nav reasoning/thinking level shown in the composer.
    pub thinking_level: String,
    /// Model context window from settings, used by future budget checks.
    pub context_window: Option<u64>,
    /// Provider/model compatibility metadata. May include an optional local
    /// tokenizer path for HF-tokenizer estimates.
    pub compat: Option<Value>,
    /// Provider-specific thinking level map, when the model exposes thinking
    /// levels under names different from nav's UI.
    pub thinking_level_map: Option<Value>,
}

impl From<ResolvedModelConfig> for OpenAiConfig {
    /// Build provider connection settings from a resolved settings.json model.
    /// The settings resolver (#531) already validated the API as
    /// `openai-completions`, so only the connection fields are carried over.
    fn from(config: ResolvedModelConfig) -> Self {
        Self {
            api_key: config.api_key,
            model: config.model,
            base_url: config.base_url,
            name: config.name,
            reasoning: config.reasoning,
            thinking_level: config.thinking_level,
            context_window: config.context_window,
            compat: config.compat,
            thinking_level_map: config.thinking_level_map,
        }
    }
}

impl OpenAiConfig {
    fn requires_reasoning_content(&self) -> bool {
        self.compat
            .as_ref()
            .and_then(|compat| compat.get("requiresReasoningContentOnAssistantMessages"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn supports_reasoning_effort(&self) -> bool {
        self.compat
            .as_ref()
            .and_then(|compat| compat.get("supportsReasoningEffort"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    fn thinking_format(&self) -> &str {
        self.compat
            .as_ref()
            .and_then(|compat| compat.get("thinkingFormat"))
            .and_then(Value::as_str)
            .unwrap_or("openai")
    }

    fn provider_thinking_level(&self) -> Option<String> {
        if !self.reasoning {
            return None;
        }

        self.map_thinking_level(&self.thinking_level)
    }

    fn map_thinking_level(&self, level: &str) -> Option<String> {
        match self
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get(level))
        {
            Some(Value::String(mapped)) => Some(mapped.clone()),
            Some(_) => None,
            None if level == "off" => None,
            None => Some(level.to_owned()),
        }
    }
}

fn apply_reasoning_settings(body: &mut Value, config: &OpenAiConfig) {
    if !config.reasoning {
        return;
    }

    let effort = config.provider_thinking_level();
    let thinking_enabled = effort.is_some();
    match config.thinking_format() {
        "deepseek" => {
            body["thinking"] = json!({ "type": thinking_type(thinking_enabled) });
            set_reasoning_effort(body, effort);
        }
        "openrouter" => {
            if let Some(effort) = effort.or_else(|| config.map_thinking_level("off")) {
                body["reasoning"] = json!({ "effort": effort });
            }
        }
        "together" => {
            body["reasoning"] = json!({ "enabled": thinking_enabled });
            if config.supports_reasoning_effort() {
                set_reasoning_effort(body, effort);
            }
        }
        "zai" | "qwen" => {
            body["enable_thinking"] = Value::Bool(thinking_enabled);
        }
        "qwen-chat-template" => {
            body["chat_template_kwargs"] = json!({
                "enable_thinking": thinking_enabled,
                "preserve_thinking": true,
            });
        }
        "string-thinking" => {
            if let Some(effort) = effort.or_else(|| config.map_thinking_level("off")) {
                body["thinking"] = Value::String(effort);
            }
        }
        _ => {
            if config.supports_reasoning_effort() {
                set_reasoning_effort(body, effort.or_else(|| config.map_thinking_level("off")));
            }
        }
    }
}

fn thinking_type(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn set_reasoning_effort(body: &mut Value, effort: Option<String>) {
    if let Some(effort) = effort {
        body["reasoning_effort"] = Value::String(effort);
    }
}

/// Real text model: one non-streaming `POST /chat/completions` call.
pub struct OpenAiModel {
    config: OpenAiConfig,
    token_counter: Arc<dyn TextTokenCounter>,
}

impl OpenAiModel {
    pub fn new(config: OpenAiConfig) -> Self {
        let token_counter = counter_from_compat(&config.model, config.compat.as_ref());
        Self {
            config,
            token_counter,
        }
    }
}

impl ChatModel for OpenAiModel {
    fn respond(
        &self,
        context: &ModelContext,
        tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        let mut messages: Vec<Value> = Vec::with_capacity(context.messages().len() + 1);
        // Mirror pi: the system prompt rides ahead of the conversation as a
        // leading `system` message.
        if let Some(system_prompt) = context.system_prompt() {
            messages.push(json!({ "role": "system", "content": system_prompt }));
        }
        let include_reasoning_content = self.config.requires_reasoning_content();
        messages.extend(
            context
                .messages()
                .iter()
                .map(|message| message_json(message, include_reasoning_content)),
        );
        let mut body = json!({ "model": self.config.model, "messages": messages });
        // Some OpenAI-compatible providers reject an empty `tools` array, so the
        // key is sent only when tools are actually offered.
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.iter().map(tool_json).collect());
        }
        apply_reasoning_settings(&mut body, &self.config);
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        let mut response = ureq::post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .send_json(&body)
            .map_err(|error| ModelError::new(format!("model request failed: {error}")))?;

        let payload: Value = response
            .body_mut()
            .read_json()
            .map_err(|error| ModelError::new(format!("could not read model response: {error}")))?;

        parse_chat_completion(&payload)
    }

    fn estimate_context_tokens(&self, context: &ModelContext, tools: &[ToolDef]) -> TokenEstimate {
        estimate_model_context(context, tools, self.token_counter.as_ref())
    }

    fn estimate_output_tokens(&self, response: &ModelResponse) -> TokenEstimate {
        estimate_assistant_output(
            response.content.as_deref(),
            response.reasoning_content.as_deref(),
            &response.tool_calls,
            self.token_counter.as_ref(),
        )
    }
}

/// Serialize one Model Context message into OpenAI chat-completions wire shape.
fn message_json(message: &ChatMessage, include_reasoning_content: bool) -> Value {
    match message.role {
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id.as_deref().unwrap_or_default(),
            "content": message.content,
        }),
        Role::Assistant if !message.tool_calls.is_empty() => {
            let tool_calls: Vec<Value> = message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": { "name": call.name, "arguments": call.arguments },
                    })
                })
                .collect();
            // A pure tool-call turn has no text; providers expect null there.
            let content = if message.content.is_empty() {
                Value::Null
            } else {
                Value::String(message.content.clone())
            };
            let mut payload =
                json!({ "role": "assistant", "content": content, "tool_calls": tool_calls });
            if include_reasoning_content && let Some(reasoning_content) = &message.reasoning_content
            {
                payload["reasoning_content"] = Value::String(reasoning_content.clone());
            }
            payload
        }
        Role::Assistant => {
            let mut payload = json!({ "role": "assistant", "content": message.content });
            if include_reasoning_content && let Some(reasoning_content) = &message.reasoning_content
            {
                payload["reasoning_content"] = Value::String(reasoning_content.clone());
            }
            payload
        }
        Role::User => json!({ "role": "user", "content": message.content }),
    }
}

/// Serialize one tool definition into the OpenAI `tools` entry shape.
fn tool_json(tool: &ToolDef) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        },
    })
}

/// Parse a chat-completions payload into a [`ModelResponse`]. A response that
/// carries neither text nor tool calls is treated as malformed.
fn parse_chat_completion(payload: &Value) -> Result<ModelResponse, ModelError> {
    let unexpected = || ModelError::new(format!("unexpected model response: {payload}"));

    let choice = payload
        .get("choices")
        .and_then(|choices| choices.get(0))
        .ok_or_else(unexpected)?;
    let message = choice.get("message").ok_or_else(unexpected)?;

    let content = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let reasoning_content = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .map(str::to_owned);
    // A malformed tool call must fail the parse rather than be silently dropped:
    // skipping one would make the run continue without executing a tool the model
    // asked for, and an empty id breaks the follow-up `tool` message OpenAI
    // requires to reference it.
    let tool_calls = match message.get("tool_calls").and_then(Value::as_array) {
        Some(calls) => calls
            .iter()
            .map(parse_tool_call)
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };

    if content.is_none() && tool_calls.is_empty() {
        return Err(unexpected());
    }

    let finish_reason = match choice.get("finish_reason").and_then(Value::as_str) {
        Some("stop") => FinishReason::Stop,
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        Some(other) => FinishReason::Other(other.to_owned()),
        None if !tool_calls.is_empty() => FinishReason::ToolCalls,
        None => FinishReason::Stop,
    };

    Ok(ModelResponse {
        content,
        reasoning_content,
        tool_calls,
        finish_reason,
        token_usage: parse_token_usage(payload),
    })
}

fn parse_token_usage(payload: &Value) -> Option<TokenUsage> {
    let usage = payload.get("usage")?;
    let input = usage_u64(usage, "prompt_tokens").unwrap_or(0);
    let output = usage_u64(usage, "completion_tokens").unwrap_or(0);
    let total = usage_u64(usage, "total_tokens");
    let reasoning = usage
        .get("completion_tokens_details")
        .and_then(|details| usage_u64(details, "reasoning_tokens"))
        .unwrap_or(0);
    let cache_read = usage
        .get("prompt_tokens_details")
        .and_then(|details| usage_u64(details, "cached_tokens"))
        .unwrap_or(0);
    let cache_write = usage
        .get("prompt_tokens_details")
        .and_then(|details| usage_u64(details, "cache_write_tokens"))
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|details| usage_u64(details, "cache_creation_tokens"))
        })
        .unwrap_or(0);

    let saw_usage = input > 0
        || output > 0
        || reasoning > 0
        || cache_read > 0
        || cache_write > 0
        || total.is_some();
    saw_usage.then(|| {
        TokenUsage::provider_reported(input, output, reasoning, cache_read, cache_write, total)
    })
}

fn usage_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|number| {
        number.as_u64().or_else(|| {
            number
                .as_i64()
                .and_then(|signed| u64::try_from(signed).ok())
        })
    })
}

fn parse_tool_call(value: &Value) -> Result<ToolCall, ModelError> {
    let malformed = || ModelError::new(format!("unexpected tool call: {value}"));

    let function = value.get("function").ok_or_else(malformed)?;
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(malformed)?
        .to_owned();
    let arguments = function
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}")
        .to_owned();
    // OpenAI requires a non-empty id so the matching `tool` result can reference
    // it; reject anything that wouldn't round-trip.
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(malformed)?
        .to_owned();
    Ok(ToolCall {
        id,
        name,
        arguments,
    })
}

/// Stand-in used when no usable model is configured; every turn fails with a
/// fixed explanation (the not-configured hint, or a specific config error).
struct FailingModel {
    message: String,
}

impl FailingModel {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl ChatModel for FailingModel {
    fn respond(
        &self,
        _context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        Err(ModelError::new(self.message.clone()))
    }
}

/// Deterministic stand-in model for tests and offline UI smoke.
///
/// Its reply echoes the latest user message and references earlier turns, so a
/// follow-up visibly proves the backend forwarded prior conversation context.
/// It never requests tools, so it drives the loop through the plain text path.
pub struct MockModel;

impl MockModel {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatModel for MockModel {
    fn respond(
        &self,
        context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        let user_messages: Vec<&str> = context
            .messages()
            .iter()
            .filter(|message| message.role == Role::User)
            .map(|message| message.content.as_str())
            .collect();

        let latest = user_messages.last().copied().unwrap_or("");
        let mut reply = format!("[mock] You said: \"{latest}\"");

        // On a follow-up, recall the opening turn so multi-turn context is
        // visibly proven without a real model.
        if user_messages.len() > 1 {
            reply.push_str(&format!(". Earlier you said: \"{}\"", user_messages[0]));
        }

        Ok(ModelResponse::text(reply))
    }
}
