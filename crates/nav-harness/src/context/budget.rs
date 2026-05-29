//! Hybrid token estimator and active-context-size formula.
//!
//! Estimates tokens using `chars / 3.8` for standard text and `chars / 2.0`
//! for dense JSON tool inputs/outputs. The active context size is computed as
//! the exact `usage.input_tokens` from the last provider response plus a
//! heuristic estimate for messages appended after it.

use crate::sessions::{Part, TokenUsage};

/// Characters per token for natural-language text (prose, thinking, etc.).
const TEXT_CHARS_PER_TOKEN: f64 = 3.8;

/// Characters per token for dense JSON (tool arguments, tool results).
const DENSE_CHARS_PER_TOKEN: f64 = 2.0;

/// Estimate the total token count for a slice of [`Part`]s.
///
/// Text and thinking parts use the natural-language ratio (chars/3.8).
/// Tool-call arguments and tool-result content use the dense-JSON ratio
/// (chars/2.0). Other part variants (step start/finish, images, etc.)
/// contribute zero tokens at this estimation level.
pub fn estimate_tokens_for_parts(parts: &[Part]) -> u64 {
    parts.iter().map(estimate_tokens_for_part).sum()
}

/// Compute the active context size using the hybrid formula:
///
/// `active = last_provider_usage.input_tokens + heuristic(parts appended after)`
pub fn active_context_size(last_usage: &TokenUsage, appended_parts: &[Part]) -> u64 {
    last_usage.input + estimate_tokens_for_parts(appended_parts)
}

fn estimate_tokens_for_part(part: &Part) -> u64 {
    match part {
        Part::Text { text, .. } | Part::Thinking { text, .. } => {
            estimate_chars(text, TEXT_CHARS_PER_TOKEN)
        }
        Part::ToolCall { arguments, .. } => estimate_chars(&arguments.to_string(), DENSE_CHARS_PER_TOKEN),
        Part::ToolResult { content, .. } => estimate_chars(content, DENSE_CHARS_PER_TOKEN),
        _ => 0,
    }
}

/// Estimate tokens as `ceil(char_count / ratio)`.
fn estimate_chars(text: &str, chars_per_token: f64) -> u64 {
    if text.is_empty() {
        return 0;
    }
    (text.chars().count() as f64 / chars_per_token).ceil() as u64
}
