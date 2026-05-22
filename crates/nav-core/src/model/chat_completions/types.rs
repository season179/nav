//! ChatCompletions-specific structs that map into the shared
//! [`crate::model::responses::types::ResponseEnvelope`] surface.
//!
//! The accumulator normalizes CC SSE chunks into Responses-shaped events, so
//! the collected envelope is structurally identical to what the Responses
//! backend produces. No CC-specific types are needed today — the parser
//! (C2) works entirely through the shared types and delegates to the
//! Responses parser for identical operations.
