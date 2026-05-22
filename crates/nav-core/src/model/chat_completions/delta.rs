//! Normalize Chat Completions SSE events into the shape the rest of the agent
//! loop already understands.
//!
//! Filled in by F2. Chat Completions emits `chat.completion.chunk` frames with
//! `choices[].delta.{role,content,tool_calls}` segments; F2 maps those into the
//! Responses-style events (`response.output_text.delta`,
//! `response.output_item.done`, `response.completed`, …) so
//! [`crate::model::responses::collector::ResponseCollector`] —
//! and our existing UI/streaming code paths — can ingest both backends.

use anyhow::Result;
use serde_json::Value;

/// Convert a single raw `chat.completion.chunk` SSE event into the
/// Responses-style event(s) the downstream collector expects.
///
/// Returning a `Vec` (instead of a single `Value`) keeps the door open for
/// one chunk to fan out into multiple normalized events — e.g. an opening
/// `response.output_item.added` plus a `response.output_text.delta` on the
/// same upstream frame.
///
/// Stub: filled in by F2.
#[allow(dead_code)]
pub(crate) fn normalize_event(_raw: &Value) -> Result<Vec<Value>> {
    unimplemented!("Chat Completions delta normalization lands in F2")
}
