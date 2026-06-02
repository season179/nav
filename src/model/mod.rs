//! Text-model abstraction for the chat/agent loop.
//!
//! A [`ChatModel`] turns assembled Model Context (plus the tools it may call)
//! into one assistant turn: free text, one or more tool calls, or both. The
//! pieces live in focused submodules:
//!
//! - [`chat`] — the request/response domain types and the [`ChatModel`] trait
//!   every adapter implements.
//! - [`choice`] — [`ModelChoice`], which resolves settings/environment into a
//!   concrete model, plus the renderer-facing [`ModelInfo`] summary.
//! - [`openai`] — the real OpenAI-compatible adapters and their wire
//!   serialization.
//! - [`mock`] — the deterministic [`MockModel`] for tests and offline smoke,
//!   and the `FailingModel` used when no model is configured.

mod chat;
mod choice;
mod mock;
mod openai;

pub use chat::{
    ChatMessage, ChatModel, FinishReason, ModelError, ModelResponse, ProviderCallTrace,
    ResponseReasoningItem, Role, ToolCall, ToolDef, TracedModelResponse,
};
pub use choice::{ModelChoice, ModelInfo, TokenBudgetInfo};
pub use mock::MockModel;
pub use openai::{OpenAiConfig, OpenAiModel, OpenAiResponsesModel};
