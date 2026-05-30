//! Context loading, memory, compaction, citations, refresh, and discard rules.

pub mod budget;
pub mod files;
pub mod reminders;
pub mod system_prompt;

use crate::sessions::{ModelTurn, Part, Turn, TurnPart};

pub use budget::{
    ContextBudget, active_context_size, estimate_dense_tokens, estimate_image_tokens,
    estimate_text_tokens, estimate_tokens_for_parts,
};
pub use files::ContextFileCache;
pub use reminders::ContextReminders;
pub use system_prompt::{Clock, Cwd, SystemClock, SystemCwd, SystemPromptBuilder};

/// Retain the newest turns that fit inside the conversation body budget.
///
/// The latest turn is always kept so the active prompt is never erased. Older
/// turns are retained contiguously from the tail until adding another turn would
/// exceed [`ContextBudget::body_budget`].
pub fn truncate(turns: Vec<(Turn, Vec<Part>)>, budget: ContextBudget) -> Vec<(Turn, Vec<Part>)> {
    let body_budget = budget.body_budget();
    let mut token_count = 0u64;
    let mut retained = Vec::new();

    for turn in turns.into_iter().rev() {
        let turn_tokens = estimate_tokens_for_parts(&turn.1).max(1);
        let must_keep_latest = retained.is_empty();
        let next_token_count = token_count.saturating_add(turn_tokens);

        if must_keep_latest || next_token_count <= body_budget {
            token_count = next_token_count;
            retained.push(turn);
        } else {
            break;
        }
    }

    retained.reverse();
    retained
}

pub fn truncate_model_turns(turns: Vec<ModelTurn>, budget: ContextBudget) -> Vec<ModelTurn> {
    let body_budget = budget.body_budget();
    let mut token_count = 0u64;
    let mut retained = Vec::new();

    for turn in turns.into_iter().rev() {
        let turn_tokens = estimate_tokens_for_model_turn(&turn).max(1);
        let must_keep_latest = retained.is_empty();
        let next_token_count = token_count.saturating_add(turn_tokens);

        if must_keep_latest || next_token_count <= body_budget {
            token_count = next_token_count;
            retained.push(turn);
        } else {
            break;
        }
    }

    retained.reverse();
    retained
}

fn estimate_tokens_for_model_turn(turn: &ModelTurn) -> u64 {
    turn.parts
        .iter()
        .map(|part| match part {
            TurnPart::Text { text, .. } => estimate_text_tokens(text),
            TurnPart::ToolCall(tool_call) => estimate_dense_tokens(&tool_call.arguments),
            TurnPart::ToolResult { content, .. } => estimate_dense_tokens(content),
        })
        .sum()
}
