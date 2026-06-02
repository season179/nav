//! OpenAI-compatible model adapters and their wire (de)serialization.
//!
//! [`OpenAiConfig`] is the resolved connection config. [`OpenAiModel`] (chat
//! completions) and [`OpenAiResponsesModel`] (the Responses API, including the
//! streaming Codex backend) each make one non-incremental call and normalize the
//! reply into a [`ModelResponse`]. The free functions below build request bodies
//! and parse provider payloads.

use std::io::BufRead;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::config::{
    CODEX_RESPONSES_API, OPENAI_COMPLETIONS_API, OPENAI_RESPONSES_API, ResolvedModelConfig,
};
use crate::context::ModelContext;
use crate::tokens::{
    TextTokenCounter, TokenEstimate, TokenUsage, counter_from_compat, estimate_assistant_output,
    estimate_model_context,
};

use super::chat::{
    ChatMessage, ChatModel, FinishReason, ModelError, ModelResponse, ProviderCallTrace,
    ResponseReasoningItem, Role, ToolCall, ToolDef, TracedModelResponse,
};

/// Connection settings for an OpenAI-compatible model provider.
pub struct OpenAiConfig {
    pub api: String,
    pub api_key: String,
    /// Provider id from settings.json when this model came from configured
    /// provider/model selection. Environment-only fallback models have none.
    pub provider: Option<String>,
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
    /// ChatGPT workspace/account id required by the Codex backend, when using
    /// subscription-backed Codex auth.
    pub chatgpt_account_id: Option<String>,
    /// Plan metadata from Codex auth, used for future UI/status surfaces.
    pub chatgpt_plan_type: Option<String>,
    /// FedRAMP routing hint from Codex auth.
    pub chatgpt_fedramp: bool,
    /// Preferred Responses service tier. For Codex ChatGPT fast mode this is
    /// the request value (`priority`) rather than the user-facing label.
    pub service_tier: Option<String>,
}

impl From<ResolvedModelConfig> for OpenAiConfig {
    /// Build provider connection settings from a resolved settings.json model.
    /// The settings resolver already validated the API kind and auth material,
    /// so only the connection fields are carried over.
    fn from(config: ResolvedModelConfig) -> Self {
        Self {
            api: config.api,
            api_key: config.api_key,
            provider: Some(config.provider),
            model: config.model,
            base_url: config.base_url,
            name: config.name,
            reasoning: config.reasoning,
            thinking_level: config.thinking_level,
            context_window: config.context_window,
            compat: config.compat,
            thinking_level_map: config.thinking_level_map,
            chatgpt_account_id: config.chatgpt_account_id,
            chatgpt_plan_type: config.chatgpt_plan_type,
            chatgpt_fedramp: config.chatgpt_fedramp,
            service_tier: config.service_tier,
        }
    }
}

impl OpenAiConfig {
    pub(super) fn is_responses_api(&self) -> bool {
        matches!(
            self.api.as_str(),
            OPENAI_RESPONSES_API | CODEX_RESPONSES_API
        )
    }

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
        self.respond_with_trace(context, tools)
            .map(|traced| traced.response)
    }

    fn respond_with_trace(
        &self,
        context: &ModelContext,
        tools: &[ToolDef],
    ) -> Result<TracedModelResponse, ModelError> {
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
        let mut trace = ProviderCallTrace::new(
            OPENAI_COMPLETIONS_API,
            url.clone(),
            self.config.model.clone(),
            body.clone(),
        );

        // `http_status_as_error(false)` keeps ureq from collapsing a 4xx/5xx into
        // a bare transport error, so the provider's error body stays readable and
        // is captured into the trace below.
        let mut response = ureq::post(&url)
            .config()
            .http_status_as_error(false)
            .build()
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .send_json(&body)
            .map_err(|error| {
                let message = format!("model request failed: {error}");
                ModelError::new(message.clone())
                    .with_provider_trace(trace.clone().with_error(&message))
            })?;
        capture_status_or_error(&mut response, &mut trace)?;

        let payload: Value = response.body_mut().read_json().map_err(|error| {
            let message = format!("could not read model response: {error}");
            ModelError::new(message.clone()).with_provider_trace(trace.clone().with_error(&message))
        })?;
        trace.provider_model_id = payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        trace.response_id = payload.get("id").and_then(Value::as_str).map(str::to_owned);
        trace.response_payload = Some(payload.clone());

        parse_chat_completion(&payload)
            .map(|response| TracedModelResponse {
                response,
                provider_trace: Some(trace.clone()),
            })
            .map_err(|error| {
                let message = error.message.clone();
                error.with_provider_trace(trace.with_error(&message))
            })
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

/// Real text model: one non-streaming `POST /responses` call.
pub struct OpenAiResponsesModel {
    config: OpenAiConfig,
    token_counter: Arc<dyn TextTokenCounter>,
}

impl OpenAiResponsesModel {
    pub fn new(config: OpenAiConfig) -> Self {
        let token_counter = counter_from_compat(&config.model, config.compat.as_ref());
        Self {
            config,
            token_counter,
        }
    }
}

impl ChatModel for OpenAiResponsesModel {
    fn respond(
        &self,
        context: &ModelContext,
        tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        self.respond_with_trace(context, tools)
            .map(|traced| traced.response)
    }

    fn respond_with_trace(
        &self,
        context: &ModelContext,
        tools: &[ToolDef],
    ) -> Result<TracedModelResponse, ModelError> {
        let input = context
            .messages()
            .iter()
            .flat_map(response_input_json)
            .collect::<Vec<_>>();
        // The Codex ChatGPT backend only accepts streaming requests and rejects
        // `stream: false` with HTTP 400 ("Stream must be set to true"). nav has
        // no incremental UI, so we still consume the whole turn before returning;
        // we just read the SSE stream to its terminal `response.completed` event.
        let streaming = self.config.api == CODEX_RESPONSES_API;
        let mut body = json!({
            "model": self.config.model,
            "input": input,
            "store": false,
            "stream": streaming,
        });
        if let Some(system_prompt) = context.system_prompt() {
            body["instructions"] = Value::String(system_prompt.to_owned());
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.iter().map(responses_tool_json).collect());
            body["tool_choice"] = Value::String("auto".to_owned());
            body["parallel_tool_calls"] = Value::Bool(true);
        }
        if let Some(reasoning) = responses_reasoning_json(&self.config) {
            body["reasoning"] = reasoning;
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
        if let Some(service_tier) = &self.config.service_tier {
            body["service_tier"] = Value::String(service_tier.clone());
        }

        let url = format!("{}/responses", self.config.base_url.trim_end_matches('/'));
        let mut trace = ProviderCallTrace::new(
            &self.config.api,
            url.clone(),
            self.config.model.clone(),
            body.clone(),
        );

        // `http_status_as_error(false)` keeps ureq from collapsing a 4xx/5xx into
        // a bare transport error, so the provider's error body stays readable and
        // is captured into the trace below.
        let mut request = ureq::post(&url)
            .config()
            .http_status_as_error(false)
            .build()
            .header("Authorization", format!("Bearer {}", self.config.api_key));
        if streaming {
            request = request.header("Accept", "text/event-stream");
        }
        if let Some(account_id) = &self.config.chatgpt_account_id {
            request = request.header("ChatGPT-Account-ID", account_id);
        }
        if self.config.chatgpt_fedramp {
            request = request.header("X-OpenAI-Fedramp", "true");
        }

        let mut response = request.send_json(&body).map_err(|error| {
            let message = format!("model request failed: {error}");
            ModelError::new(message.clone()).with_provider_trace(trace.clone().with_error(&message))
        })?;
        // A non-2xx never carries the SSE stream, even for the streaming Codex
        // backend — it returns a JSON error body, which `capture_status_or_error`
        // captures before we attempt to read the success payload below.
        capture_status_or_error(&mut response, &mut trace)?;

        // Codex auth streams the response as SSE; every other Responses provider
        // returns one JSON body. Both reduce to a single payload value or an
        // error message, so the error is wrapped with the trace just once.
        let payload: Value = if streaming {
            read_responses_stream(response.body_mut().as_reader())
        } else {
            response
                .body_mut()
                .read_json()
                .map_err(|error| format!("could not read model response: {error}"))
        }
        .map_err(|message| {
            ModelError::new(message.clone()).with_provider_trace(trace.clone().with_error(&message))
        })?;
        trace.provider_model_id = payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        trace.response_id = payload.get("id").and_then(Value::as_str).map(str::to_owned);
        trace.response_payload = Some(payload.clone());

        parse_responses_payload(&payload)
            .map(|response| TracedModelResponse {
                response,
                provider_trace: Some(trace.clone()),
            })
            .map_err(|error| {
                let message = error.message.clone();
                error.with_provider_trace(trace.with_error(&message))
            })
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

/// Record the response status and request id onto the trace, then turn a non-2xx
/// into a [`ModelError`] whose message carries the provider's error body (also
/// captured onto the trace). Returns `Ok(())` on a 2xx so the caller proceeds to
/// read the success payload. Shared by every HTTP adapter so status handling and
/// error capture stay identical across providers.
fn capture_status_or_error(
    response: &mut ureq::http::Response<ureq::Body>,
    trace: &mut ProviderCallTrace,
) -> Result<(), ModelError> {
    let status = response.status().as_u16();
    trace.status_code = Some(status);
    trace.request_id = response
        .headers()
        .get("x-request-id")
        .or_else(|| response.headers().get("request-id"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    if response.status().is_success() {
        return Ok(());
    }

    let detail = capture_provider_error_body(response, trace);
    let message = format!("model request failed: http status: {status}: {detail}");
    Err(ModelError::new(message.clone()).with_provider_trace(trace.clone().with_error(&message)))
}

/// Read a non-2xx provider response body, store it on the trace (parsed as JSON
/// when possible, otherwise as raw text), and return a short detail for the
/// error message. Capturing the body is the point: the provider's explanation of
/// *why* a call failed lives here, not in the status line.
fn capture_provider_error_body(
    response: &mut ureq::http::Response<ureq::Body>,
    trace: &mut ProviderCallTrace,
) -> String {
    let text = response.body_mut().read_to_string().unwrap_or_default();
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => {
            let detail = extract_provider_error_message(&value).unwrap_or_else(|| text.clone());
            trace.response_payload = Some(value);
            detail
        }
        Err(_) => {
            if !text.is_empty() {
                trace.response_payload = Some(Value::String(text.clone()));
            }
            text
        }
    }
}

/// Pull the provider's human-facing error message out of a JSON error body,
/// trying the common OpenAI-compatible shapes.
fn extract_provider_error_message(value: &Value) -> Option<String> {
    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return Some(message.to_owned());
    }
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Some(error.to_owned());
    }
    value
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn responses_reasoning_json(config: &OpenAiConfig) -> Option<Value> {
    if !config.reasoning || !config.supports_reasoning_effort() {
        return None;
    }

    responses_reasoning_effort(config).map(|effort| json!({ "effort": effort }))
}

fn responses_reasoning_effort(config: &OpenAiConfig) -> Option<String> {
    if config.thinking_level == "off" {
        return config
            .thinking_level_map
            .as_ref()
            .and_then(|map| map.get("off"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| Some("none".to_owned()));
    }

    config.provider_thinking_level()
}

fn response_input_json(message: &ChatMessage) -> Vec<Value> {
    match message.role {
        Role::User => vec![responses_message_json(
            "user",
            "input_text",
            &message.content,
        )],
        Role::Assistant if message.tool_calls.is_empty() => {
            let mut items = responses_reasoning_items_json(&message.response_reasoning_items);
            items.push(responses_message_json(
                "assistant",
                "output_text",
                &message.content,
            ));
            items
        }
        Role::Assistant => {
            let mut items = responses_reasoning_items_json(&message.response_reasoning_items);
            if !message.content.is_empty() {
                items.push(responses_message_json(
                    "assistant",
                    "output_text",
                    &message.content,
                ));
            }
            items.extend(message.tool_calls.iter().map(responses_function_call_json));
            items
        }
        Role::Tool => vec![json!({
            "type": "function_call_output",
            "call_id": message.tool_call_id.as_deref().unwrap_or_default(),
            "output": message.content,
        })],
    }
}

fn responses_reasoning_items_json(reasoning: &[ResponseReasoningItem]) -> Vec<Value> {
    // The Responses API requires a `summary` array on replayed reasoning items
    // and rejects the server-assigned `id` under `store: false` (the same class
    // of error the function-call `id` hit). nav does not retain per-item summary
    // text, so send an empty `summary` — the `encrypted_content` carries the
    // full reasoning. This mirrors codex-rs, which skips the id and always emits
    // `summary`.
    reasoning
        .iter()
        .map(|item| {
            json!({
                "type": "reasoning",
                "summary": [],
                "encrypted_content": item.encrypted_content,
            })
        })
        .collect()
}

fn responses_message_json(role: &str, content_type: &str, text: &str) -> Value {
    json!({
        "type": "message",
        "role": role,
        "content": [{ "type": content_type, "text": text }],
    })
}

fn responses_function_call_json(call: &ToolCall) -> Value {
    // Only `call_id` is echoed back to pair the call with its output. We do not
    // retain the server-assigned output-item id (`fc_...`), and supplying the
    // `call_...` id in the `id` field makes the Codex backend reject the request
    // ("Invalid 'input[..].id': Expected an ID that begins with 'fc'").
    json!({
        "type": "function_call",
        "call_id": call.id,
        "name": call.name,
        "arguments": call.arguments,
    })
}

fn responses_tool_json(tool: &ToolDef) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
    })
}

/// Serialize one Model Context message into OpenAI chat-completions wire shape.
pub(super) fn message_json(message: &ChatMessage, include_reasoning_content: bool) -> Value {
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
        response_reasoning_items: Vec::new(),
        tool_calls,
        finish_reason,
        token_usage: parse_token_usage(payload),
    })
}

/// Drain a Responses Server-Sent Events stream and return the final response
/// object in the same shape as a non-streaming body, so [`parse_responses_payload`]
/// can consume it unchanged. Codex-backed ChatGPT auth only streams, and its
/// terminal `response.completed` event omits the `output` array: each output
/// item (messages, reasoning, function calls) arrives on its own
/// `response.output_item.done` event instead, so we assemble the array here.
/// `response.failed` / `error` events surface as the call error.
fn read_responses_stream(reader: impl std::io::Read) -> Result<Value, String> {
    let reader = std::io::BufReader::new(reader);
    let mut completed: Option<Value> = None;
    let mut output_items: Vec<Value> = Vec::new();
    let mut stream_error: Option<String> = None;

    for line in reader.lines() {
        let line = line.map_err(|error| format!("could not read model response: {error}"))?;
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_item.done") => {
                if let Some(item) = event.get("item") {
                    output_items.push(item.clone());
                }
            }
            Some("response.completed") => {
                if let Some(response) = event.get("response") {
                    completed = Some(response.clone());
                }
            }
            Some("response.failed") => {
                stream_error = Some(stream_error_message(
                    event
                        .get("response")
                        .and_then(|response| response.get("error")),
                    "model stream reported a failure",
                ));
            }
            Some("error") => {
                stream_error = Some(stream_error_message(
                    event.get("error").or(Some(&event)),
                    "model stream returned an error",
                ));
            }
            _ => {}
        }
    }

    if let Some(mut response) = completed {
        // The terminal event usually omits `output`; fall back to the items we
        // assembled from the per-item events so downstream parsing sees a body
        // identical to the non-streaming response shape.
        let has_output = response
            .get("output")
            .and_then(Value::as_array)
            .is_some_and(|output| !output.is_empty());
        if !has_output && !output_items.is_empty() {
            response["output"] = Value::Array(output_items);
        }
        return Ok(response);
    }
    Err(stream_error
        .unwrap_or_else(|| "model stream ended without a completed response".to_owned()))
}

fn stream_error_message(error: Option<&Value>, fallback: &str) -> String {
    error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| fallback.to_owned())
}

fn parse_responses_payload(payload: &Value) -> Result<ModelResponse, ModelError> {
    let unexpected = || ModelError::new(format!("unexpected model response: {payload}"));
    let output = payload
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(unexpected)?;

    let content = response_output_text(output);
    let response_reasoning = response_reasoning(output);
    let tool_calls = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .map(parse_response_function_call)
        .collect::<Result<Vec<_>, _>>()?;

    if content.is_none() && tool_calls.is_empty() {
        return Err(unexpected());
    }

    let finish_reason = response_finish_reason(payload, !tool_calls.is_empty());

    Ok(ModelResponse {
        content,
        reasoning_content: response_reasoning.summary,
        response_reasoning_items: response_reasoning.items,
        tool_calls,
        finish_reason,
        token_usage: parse_responses_token_usage(payload),
    })
}

fn response_output_text(output: &[Value]) -> Option<String> {
    let mut text = String::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if let Some("output_text" | "text") = part.get("type").and_then(Value::as_str)
                && let Some(part_text) = part.get("text").and_then(Value::as_str)
            {
                text.push_str(part_text);
            }
        }
    }

    (!text.is_empty()).then_some(text)
}

struct ParsedResponseReasoning {
    summary: Option<String>,
    items: Vec<ResponseReasoningItem>,
}

fn response_reasoning(output: &[Value]) -> ParsedResponseReasoning {
    let mut text = String::new();
    let mut items = Vec::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        if let (Some(id), Some(encrypted_content)) = (
            item.get("id").and_then(Value::as_str),
            item.get("encrypted_content").and_then(Value::as_str),
        ) && !id.is_empty()
            && !encrypted_content.is_empty()
        {
            items.push(ResponseReasoningItem {
                id: id.to_owned(),
                encrypted_content: encrypted_content.to_owned(),
            });
        }
        let Some(summary) = item.get("summary").and_then(Value::as_array) else {
            continue;
        };
        for part in summary {
            if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(part_text);
            }
        }
    }

    ParsedResponseReasoning {
        summary: (!text.is_empty()).then_some(text),
        items,
    }
}

fn parse_response_function_call(item: &Value) -> Result<ToolCall, ModelError> {
    let malformed = || ModelError::new(format!("unexpected function call: {item}"));
    let id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(malformed)?
        .to_owned();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(malformed)?
        .to_owned();
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}")
        .to_owned();

    Ok(ToolCall {
        id,
        name,
        arguments,
    })
}

fn response_finish_reason(payload: &Value, has_tool_calls: bool) -> FinishReason {
    if has_tool_calls {
        return FinishReason::ToolCalls;
    }

    match payload
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
    {
        Some("max_output_tokens" | "max_tokens") => FinishReason::Length,
        Some(reason) => FinishReason::Other(reason.to_owned()),
        None => FinishReason::Stop,
    }
}

fn parse_token_usage(payload: &Value) -> Option<TokenUsage> {
    parse_usage(
        payload,
        UsageKeys {
            input: "prompt_tokens",
            output: "completion_tokens",
            input_details: "prompt_tokens_details",
            output_details: "completion_tokens_details",
        },
    )
}

fn parse_responses_token_usage(payload: &Value) -> Option<TokenUsage> {
    parse_usage(
        payload,
        UsageKeys {
            input: "input_tokens",
            output: "output_tokens",
            input_details: "input_tokens_details",
            output_details: "output_tokens_details",
        },
    )
}

struct UsageKeys {
    input: &'static str,
    output: &'static str,
    input_details: &'static str,
    output_details: &'static str,
}

fn parse_usage(payload: &Value, keys: UsageKeys) -> Option<TokenUsage> {
    let usage = payload.get("usage")?;
    let input = usage_u64(usage, keys.input).unwrap_or(0);
    let output = usage_u64(usage, keys.output).unwrap_or(0);
    let total = usage_u64(usage, "total_tokens");
    let reasoning = usage_details_u64(usage, keys.output_details, "reasoning_tokens").unwrap_or(0);
    let cache_read = usage_details_u64(usage, keys.input_details, "cached_tokens").unwrap_or(0);
    let cache_write = usage_details_u64(usage, keys.input_details, "cache_write_tokens")
        .or_else(|| usage_details_u64(usage, keys.input_details, "cache_creation_tokens"))
        .unwrap_or(0);

    has_reported_usage(input, output, reasoning, cache_read, cache_write, total).then(|| {
        TokenUsage::provider_reported(input, output, reasoning, cache_read, cache_write, total)
    })
}

fn usage_details_u64(usage: &Value, details_key: &str, value_key: &str) -> Option<u64> {
    usage
        .get(details_key)
        .and_then(|details| usage_u64(details, value_key))
}

fn has_reported_usage(
    input: u64,
    output: u64,
    reasoning: u64,
    cache_read: u64,
    cache_write: u64,
    total: Option<u64>,
) -> bool {
    input > 0 || output > 0 || reasoning > 0 || cache_read > 0 || cache_write > 0 || total.is_some()
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
