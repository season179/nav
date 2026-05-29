//! Anti-thrashing breaker for low-savings automatic compactions.

/// Fraction of context a single pass must free to count as useful savings.
const LOW_SAVINGS_THRESHOLD: f64 = 0.10;
/// Consecutive low-savings passes that pause automatic compaction.
const LOW_SAVINGS_LIMIT: u32 = 2;

/// Fraction of context freed by a compaction pass, clamped to `[0.0, 1.0]`.
/// A pass that does not shrink the context (or an empty context) yields `0.0`.
pub fn savings_ratio(tokens_before: usize, tokens_after: usize) -> f64 {
    if tokens_before == 0 {
        return 0.0;
    }
    let freed = tokens_before.saturating_sub(tokens_after);
    freed as f64 / tokens_before as f64
}

/// Tracks consecutive low-savings compaction passes for a session and decides
/// whether further automatic compaction should be paused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AntiThrashingBreaker {
    consecutive_low_savings: u32,
}

/// Whether the orchestration loop should run the next automatic compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoCompactionDecision {
    /// Automatic compaction may proceed.
    Proceed,
    /// Automatic compaction is paused; surface `warning` to the user.
    Skip { warning: String },
}

impl AntiThrashingBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the savings achieved by a completed automatic compaction pass.
    pub fn record_auto_compaction(&mut self, savings: f64) {
        if savings < LOW_SAVINGS_THRESHOLD {
            self.consecutive_low_savings = self.consecutive_low_savings.saturating_add(1);
        } else {
            self.consecutive_low_savings = 0;
        }
    }

    /// Clear the counter after a manual compaction or `/new`.
    pub fn reset(&mut self) {
        self.consecutive_low_savings = 0;
    }

    /// Decide whether the next automatic compaction should run.
    pub fn decide_auto_compaction(&self) -> AutoCompactionDecision {
        if self.consecutive_low_savings >= LOW_SAVINGS_LIMIT {
            AutoCompactionDecision::Skip {
                warning: low_savings_warning(),
            }
        } else {
            AutoCompactionDecision::Proceed
        }
    }
}

fn low_savings_warning() -> String {
    format!(
        "Automatic compaction paused: the last {LOW_SAVINGS_LIMIT} passes each freed less \
         than {}% of the context. Start a new session or compact manually to resume.",
        LOW_SAVINGS_THRESHOLD * 100.0
    )
}
