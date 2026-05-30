//! Hybrid token estimator and active-context-size formula.
//!
//! Estimates tokens using `chars / 3.8` for standard text, `chars / 2.0` for
//! dense JSON tool inputs/outputs, and a pixel-based (or conservative flat)
//! estimate for images. The active context size is computed as the exact
//! `usage.input_tokens` from the last provider response plus a heuristic
//! estimate for messages appended after it.

use crate::models::ModelConfig;
use crate::sessions::{ModelTurn, Part, TokenUsage, TurnPart};

/// Characters per token for natural-language text (prose, thinking, etc.).
const TEXT_CHARS_PER_TOKEN: f64 = 3.8;

/// Characters per token for dense JSON (tool arguments, tool results).
const DENSE_CHARS_PER_TOKEN: f64 = 2.0;

/// Pixels per token for the pixel-based image estimate
/// (provider-documented `tokens ≈ width × height / 750`).
const IMAGE_PIXELS_PER_TOKEN: u64 = 750;

/// Conservative flat token estimate for a resized image whose pixel
/// dimensions are unknown. Deliberately errs high: under-counting media is the
/// single most common cause of a session reading "well under threshold" yet
/// still hitting a hard `prompt_too_long`.
const IMAGE_FLAT_FALLBACK_TOKENS: u64 = 1600;

/// Completion buffer used when a model config does not pin `maxTokens`.
pub const DEFAULT_COMPLETION_BUFFER_TOKENS: u64 = 4_096;

/// Estimate the total token count for a slice of [`Part`]s.
///
/// Text and thinking parts use the natural-language ratio (chars/3.8).
/// Tool-call arguments and tool-result content use the dense-JSON ratio
/// (chars/2.0). Images use [`estimate_image_tokens`]. Other part variants
/// (step start/finish, etc.) contribute zero tokens at this estimation level.
pub fn estimate_tokens_for_parts(parts: &[Part]) -> u64 {
    parts.iter().map(estimate_tokens_for_part).sum()
}

/// Estimate the total token count for model-visible turns.
pub fn estimate_tokens_for_model_turns(turns: &[ModelTurn]) -> u64 {
    turns
        .iter()
        .flat_map(|turn| &turn.parts)
        .map(estimate_tokens_for_turn_part)
        .sum()
}

/// Compute the active context size using the hybrid formula:
///
/// `active = last_provider_usage.input_tokens + heuristic(parts appended after)`
pub fn active_context_size(last_usage: &TokenUsage, appended_parts: &[Part]) -> u64 {
    last_usage.input + estimate_tokens_for_parts(appended_parts)
}

/// Two-scope view of the token budget for one session.
///
/// We track usage against two windows so a large static prompt never triggers
/// premature compaction:
/// 1. **Total context budget** — the model's absolute window
///    ([`total_context`](ContextBudget::total_context)).
/// 2. **Body-after-prefix budget** — room left for the active conversation after
///    subtracting the cache-stable `prefix` (system prompt + static context
///    blocks) from the total window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    total_context: u64,
    prefix: u64,
}

impl ContextBudget {
    /// Create a budget from the model window and the prefix size (tokens for the
    /// system prompt plus static context blocks).
    pub fn new(total_context: u64, prefix: u64) -> Self {
        Self {
            total_context,
            prefix,
        }
    }

    /// Create a budget by reading the total window from a [`ModelConfig`]
    /// (falling back to its default window when unspecified) and pairing it with
    /// the measured `prefix` size.
    pub fn from_model(model: &ModelConfig, prefix: u64) -> Self {
        Self::new(model.context_window_tokens(), prefix)
    }

    /// Total context budget: the model's absolute window size in tokens.
    pub fn total_context(&self) -> u64 {
        self.total_context
    }

    /// Tokens consumed by the cache-stable prefix (system prompt + static blocks).
    pub fn prefix(&self) -> u64 {
        self.prefix
    }

    /// Body-after-prefix budget: tokens left for the conversation body once the
    /// static prefix is subtracted from the total window. Saturates at zero when
    /// the prefix alone exceeds the window.
    pub fn body_budget(&self) -> u64 {
        self.total_context.saturating_sub(self.prefix)
    }

    /// Body size implied by `active_context_size`: the active context with the
    /// static prefix subtracted out. Saturates at zero when the active size is
    /// smaller than the prefix.
    pub fn body_after_prefix(&self, active_context_size: u64) -> u64 {
        active_context_size.saturating_sub(self.prefix)
    }

    /// Cheap pruning should begin once the conversation body exceeds roughly
    /// 60% of the body-after-prefix budget.
    pub fn prune_threshold(&self) -> u64 {
        self.body_budget().saturating_mul(60) / 100
    }

    /// Usable body threshold: the body budget with room reserved for the model
    /// completion.
    pub fn usable_threshold(&self, completion_buffer: u64) -> u64 {
        self.body_budget().saturating_sub(completion_buffer)
    }
}

fn estimate_tokens_for_part(part: &Part) -> u64 {
    match part {
        Part::Text { text, .. } | Part::Thinking { text, .. } => estimate_text_tokens(text),
        Part::ToolCall { arguments, .. } => estimate_dense_tokens(&arguments.to_string()),
        Part::ToolResult { content, .. } => estimate_dense_tokens(content),
        Part::Image { .. } => estimate_image_tokens(None),
        _ => 0,
    }
}

fn estimate_tokens_for_turn_part(part: &TurnPart) -> u64 {
    match part {
        TurnPart::Text { text, .. } => estimate_text_tokens(text),
        TurnPart::ToolCall(tool_call) => estimate_dense_tokens(&tool_call.arguments),
        TurnPart::ToolResult { content, .. } => estimate_dense_tokens(content),
    }
}

/// Estimate natural-language text tokens using the hybrid text ratio.
pub fn estimate_text_tokens(text: &str) -> u64 {
    estimate_chars(text, TEXT_CHARS_PER_TOKEN)
}

/// Estimate dense JSON/tool text tokens using the hybrid dense ratio.
pub fn estimate_dense_tokens(text: &str) -> u64 {
    estimate_chars(text, DENSE_CHARS_PER_TOKEN)
}

/// Estimate the token cost of an image, rounding up.
///
/// When pixel `dimensions` are known, uses the `width × height / 750`
/// formula; otherwise falls back to a conservative flat estimate. Rounding up
/// guards against the under-counting that triggers `prompt_too_long`.
pub fn estimate_image_tokens(dimensions: Option<(u32, u32)>) -> u64 {
    match dimensions {
        Some((width, height)) => {
            // Integer ceil-division: exact, with no float rounding that could
            // silently under-count a very large image.
            let pixels = u128::from(width) * u128::from(height);
            pixels.div_ceil(u128::from(IMAGE_PIXELS_PER_TOKEN)) as u64
        }
        None => IMAGE_FLAT_FALLBACK_TOKENS,
    }
}

/// Estimate tokens as `ceil(char_count / ratio)`.
fn estimate_chars(text: &str, chars_per_token: f64) -> u64 {
    if text.is_empty() {
        return 0;
    }
    (text.chars().count() as f64 / chars_per_token).ceil() as u64
}
