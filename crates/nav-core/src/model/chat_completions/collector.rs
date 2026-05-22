//! Accumulate streamed Chat Completions deltas into a single
//! [`ResponseEnvelope`] so the agent loop can consume the same shape it gets
//! from the Responses backend.
//!
//! Filled in by C2. Counterpart to
//! [`crate::model::responses::collector::ResponseCollector`].

use crate::model::responses::types::ResponseEnvelope;
use anyhow::Result;
use serde_json::Value;

#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct ChatCompletionsCollector {
    // Real fields land in C2; the shape mirrors
    // `responses::collector::ResponseCollector` (an accumulator for assistant
    // text, tool calls, and the final completion event).
}

#[allow(dead_code)]
impl ChatCompletionsCollector {
    /// Fold one normalized event into the running envelope. Returns `true`
    /// when the stream has produced a terminal event and the caller should
    /// stop reading.
    ///
    /// Stub: filled in by C2.
    pub(crate) fn push_event(&mut self, _event: &Value, _source: &str) -> Result<bool> {
        unimplemented!("Chat Completions collector lands in C2")
    }

    /// Finalize the accumulator into a [`ResponseEnvelope`].
    ///
    /// Stub: filled in by C2.
    pub(crate) fn finish(self, _source: &str) -> Result<ResponseEnvelope> {
        unimplemented!("Chat Completions collector finalization lands in C2")
    }
}
