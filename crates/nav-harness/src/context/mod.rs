//! Context loading, memory, compaction, citations, refresh, and discard rules.

pub mod budget;
pub mod files;
pub mod reminders;
pub mod system_prompt;

pub use budget::{
    ContextBudget, DEFAULT_COMPLETION_BUFFER_TOKENS, active_context_size, estimate_image_tokens,
    estimate_tokens_for_model_turns, estimate_tokens_for_parts,
};
pub use files::ContextFileCache;
pub use reminders::ContextReminders;
pub use system_prompt::{Clock, Cwd, SystemClock, SystemCwd, SystemPromptBuilder};
