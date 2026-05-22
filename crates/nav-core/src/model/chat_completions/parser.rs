//! Chat Completions response parsing.
//!
//! Filled in by C2. Mirrors the helpers in
//! [`crate::model::responses::parser`] so the agent loop can call into either
//! backend without branching on transport.

use crate::model::responses::types::ResponseEnvelope;
use anyhow::Result;
use serde_json::Value;

/// Materialize a final assistant turn from the accumulated Chat Completions
/// envelope (assistant text, tool calls, usage). Returns the same
/// [`ResponseEnvelope`] shape the Responses backend produces so downstream
/// code in `agent_loop/runner.rs` does not need to branch on transport.
///
/// Stub: filled in by C2.
#[allow(dead_code)]
pub(crate) fn process_response(_envelope: ResponseEnvelope) -> Result<Value> {
    unimplemented!("Chat Completions response processing lands in C2")
}

/// Sanitize continuation items before they are appended to the next request's
/// `input` array — strip provider-specific fields the Chat Completions API
/// won't accept on echo-back. Counterpart to
/// [`crate::model::responses::parser::sanitize_continuation_items`].
///
/// Stub: filled in by C2.
#[allow(dead_code)]
pub(crate) fn sanitize_continuation_items(_items: &mut Vec<Value>) {
    unimplemented!("Chat Completions continuation sanitization lands in C2")
}
