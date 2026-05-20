//! Compatibility exports for OpenAI Responses support.
//!
//! New code should use [`crate::model::responses`] or [`crate::model`].

pub use crate::model::responses::types;
pub use crate::model::responses::{
    OpenAiTransport, ResponsesError, RetryPolicy, ToolCall, into_raw_output, process_response,
};
