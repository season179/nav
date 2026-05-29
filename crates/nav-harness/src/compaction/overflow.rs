//! Context-overflow recovery: the synthetic continuation prompt replayed after
//! a request is rejected for exceeding the provider's context window.
//!
//! When [`crate::models::ContextLimitError`] is classified, the run loop
//! force-compacts the session, strips media from the replay projection, and
//! appends exactly one synthetic user turn carrying this prompt so the
//! continuation model knows to resume from the compaction summary rather than
//! treating the truncated history as the whole task.

/// The single synthetic user message appended after an overflow compaction.
///
/// It is replayed once per recovery; [`crate::sessions::SessionStore`] guards
/// against appending it twice in a row so the continuation prompt is never
/// duplicated.
pub const OVERFLOW_CONTINUATION_TEXT: &str = "The conversation exceeded the model's context window and was compacted. \
Continue the task from the summary above and the most recent messages without repeating completed work.";
