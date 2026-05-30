//! Context-overflow recovery after a request is rejected for exceeding the
//! provider's context window.
//!
//! When [`crate::models::ContextLimitError`] is classified, the run loop
//! force-compacts the session, strips media from the replay projection, and
//! appends exactly one synthetic user turn that replays the original triggering
//! user request. This fallback prompt remains for callers that do not have that
//! original text available.

/// Fallback synthetic user message for overflow compaction.
///
/// It is replayed once per recovery; [`crate::sessions::SessionStore`] guards
/// against appending it twice in a row so the fallback text is never
/// duplicated.
pub const OVERFLOW_CONTINUATION_TEXT: &str = "The conversation exceeded the model's context window and was compacted. \
Continue the task from the summary above and the most recent messages without repeating completed work.";
