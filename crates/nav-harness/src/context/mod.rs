//! Context loading, memory, compaction, citations, refresh, and discard rules.

pub mod budget;
pub mod files;
pub mod system_prompt;

pub use budget::{active_context_size, estimate_tokens_for_parts};
pub use files::ContextFileCache;
pub use system_prompt::{Clock, Cwd, SystemClock, SystemCwd, SystemPromptBuilder};
