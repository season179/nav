//! Shared policy for model-visible replay history.
//!
//! The active agent loop and durable resume path both need the same knobs for
//! deciding how much historical tool output may remain model-visible.

/// Replay budget shared by pre-call pruning and replay reconstruction.
/// Constants live in one place so the policy stays auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayBudget {
    /// Number of trailing user-message boundaries whose tool pairs are kept
    /// verbatim. The "active turn" — i.e. tool pairs after the latest user
    /// message — is always included.
    pub raw_tool_turns: usize,
    /// Per-output cap for old reduced `function_call_output.output` text.
    pub max_raw_tool_output_bytes: usize,
    /// Total `function_call_output.output` byte budget across the assembled
    /// input. Replay clears oldest unprotected outputs when total exceeds this.
    pub max_total_tool_output_bytes: usize,
    /// Number of trailing user-message boundaries whose `reasoning` continuation
    /// items stay attached. Older reasoning is dropped: encrypted continuation
    /// only steers the very next provider response, so once a new user message
    /// arrives the prior reasoning is dead weight.
    pub keep_reasoning_turns: usize,
    /// Number of trailing user-message boundaries whose user images stay
    /// attached. Older `input_image` parts get replaced with a placeholder so
    /// inspectors can still see what was there without paying image-token cost.
    pub keep_image_turns: usize,
}

impl Default for ReplayBudget {
    fn default() -> Self {
        Self {
            raw_tool_turns: 2,
            max_raw_tool_output_bytes: 50 * 1024,
            max_total_tool_output_bytes: 120 * 1024,
            keep_reasoning_turns: 1,
            keep_image_turns: 2,
        }
    }
}
