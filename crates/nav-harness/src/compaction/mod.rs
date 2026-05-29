//! Context compaction and replay projection.

pub mod breaker;
pub mod degrade;
pub mod overflow;
pub mod prune;
pub mod replay;
pub mod summary;
pub mod validate;

pub const COMPACTION_REPLAY_TEXT: &str =
    "Context was compacted. Previous conversation history has been summarized.";
pub const COMPACTION_SUMMARY_PLACEHOLDER: &str = "[Compaction summary pending]";
