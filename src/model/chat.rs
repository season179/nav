//! Core chat domain types and the [`ChatModel`] trait.
//!
//! These are the request/response shapes every model adapter speaks in: a
//! [`ChatMessage`] history assembled into [`ModelContext`], the [`ToolDef`]s a
//! model may call, and the [`ModelResponse`] (text, tool calls, or both) it
//! returns. [`ChatModel`] is the single interface the mock and the real
//! OpenAI-compatible adapters implement.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::ModelContext;
use crate::tokens::{
    HeuristicTokenCounter, TokenEstimate, TokenUsage, estimate_assistant_output,
    estimate_model_context,
};

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

/// Opaque Responses API reasoning state that must be replayed with an
/// assistant turn in stateless follow-up requests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseReasoningItem {
    pub id: String,
    pub encrypted_content: String,
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
    /// Opaque Responses API reasoning payloads for assistant turns. These are
    /// sent back to OpenAI but are not user-visible reasoning text.
    pub response_reasoning_items: Vec<ResponseReasoningItem>,
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
            response_reasoning_items: Vec::new(),
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
            response_reasoning_items: Vec::new(),
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
            response_reasoning_items: Vec::new(),
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
            response_reasoning_items: Vec::new(),
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
    /// Opaque Responses API reasoning payloads to replay on later model calls.
    pub response_reasoning_items: Vec<ResponseReasoningItem>,
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
            response_reasoning_items: Vec::new(),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            token_usage: None,
        }
    }
}

/// A normalized response paired with provider transport details, when the model
/// adapter can expose them.
#[derive(Clone, Debug)]
pub struct TracedModelResponse {
    pub response: ModelResponse,
    pub provider_trace: Option<ProviderCallTrace>,
}

impl From<ModelResponse> for TracedModelResponse {
    fn from(response: ModelResponse) -> Self {
        Self {
            response,
            provider_trace: None,
        }
    }
}

/// Provider request/response details captured around one model call.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCallTrace {
    pub api_kind: String,
    pub url: String,
    pub model_id: String,
    pub request_payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ProviderCallTrace {
    pub(super) fn new(
        api_kind: &str,
        url: String,
        model_id: String,
        request_payload: Value,
    ) -> Self {
        Self {
            api_kind: api_kind.to_owned(),
            url,
            model_id,
            request_payload,
            response_payload: None,
            provider_model_id: None,
            response_id: None,
            request_id: None,
            status_code: None,
            error: None,
        }
    }

    pub(super) fn with_error(mut self, error: &str) -> Self {
        self.error = Some(error.to_owned());
        self
    }
}

/// Why a model call failed. Surfaced to the renderer as a `run.failed` event.
#[derive(Clone, Debug)]
pub struct ModelError {
    pub message: String,
    pub provider_trace: Option<Box<ProviderCallTrace>>,
}

impl ModelError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            provider_trace: None,
        }
    }

    pub(super) fn with_provider_trace(mut self, provider_trace: ProviderCallTrace) -> Self {
        self.provider_trace = Some(Box::new(provider_trace));
        self
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

    fn respond_with_trace(
        &self,
        context: &ModelContext,
        tools: &[ToolDef],
    ) -> Result<TracedModelResponse, ModelError> {
        self.respond(context, tools).map(TracedModelResponse::from)
    }

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
