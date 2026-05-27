//! Context loading, memory, compaction, citations, refresh, and discard rules.

pub mod system_prompt;

pub use system_prompt::{Clock, Cwd, SystemClock, SystemCwd, SystemPromptBuilder};

#[derive(Debug, Default)]
pub struct ContextManager;
