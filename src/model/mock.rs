//! Offline model stand-ins.
//!
//! [`MockModel`] is the deterministic model used by tests and offline UI smoke;
//! its reply echoes the latest user message and recalls earlier turns so
//! multi-turn context is visibly proven without a real provider. [`FailingModel`]
//! backs the not-configured / unavailable cases: every turn fails with a fixed
//! explanation.

use serde_json::{Value, json};

use crate::context::ModelContext;

use super::chat::{
    ChatModel, ModelError, ModelResponse, ProviderCallTrace, Role, ToolDef, TracedModelResponse,
};
use super::openai::message_json;

/// Stand-in used when no usable model is configured; every turn fails with a
/// fixed explanation (the not-configured hint, or a specific config error).
pub(crate) struct FailingModel {
    message: String,
}

impl FailingModel {
    pub(crate) fn new(message: impl Into<String>) -> Self {
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
        tools: &[ToolDef],
    ) -> Result<ModelResponse, ModelError> {
        self.respond_with_trace(context, tools)
            .map(|traced| traced.response)
    }

    fn respond_with_trace(
        &self,
        context: &ModelContext,
        _tools: &[ToolDef],
    ) -> Result<TracedModelResponse, ModelError> {
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

        // Build a representative request/response trace so the stack-capture path
        // is exercised offline and in tests, mirroring the real adapters: the
        // request body holds the system prompt and message history, the response
        // body holds the assembled reply.
        let mut messages: Vec<Value> = Vec::with_capacity(context.messages().len() + 1);
        if let Some(system_prompt) = context.system_prompt() {
            messages.push(json!({ "role": "system", "content": system_prompt }));
        }
        messages.extend(
            context
                .messages()
                .iter()
                .map(|message| message_json(message, false)),
        );
        let request_payload = json!({ "model": "mock", "messages": messages });

        let mut trace = ProviderCallTrace::new(
            "mock",
            "mock://local".to_owned(),
            "mock".to_owned(),
            request_payload,
        );
        trace.status_code = Some(200);
        trace.response_payload = Some(json!({
            "output": [{ "role": "assistant", "content": reply }],
        }));

        Ok(TracedModelResponse {
            response: ModelResponse::text(reply),
            provider_trace: Some(trace),
        })
    }
}
