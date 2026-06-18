//! Context assembly for agent runs.
//!
//! Today this module deliberately preserves the existing behavior: every stored
//! turn is forwarded to the model in order. Naming that transformation as a
//! [`ContextStrategy`] gives future context management one seam to grow ranking,
//! pinning, summaries, citations, and pruning without spreading those decisions
//! across sessions, agents, and model adapters.

use std::sync::Arc;

use crate::model::{ChatMessage, ToolDef};
use crate::tokens::{TextTokenCounter, TokenEstimate, estimate_model_context};

/// The raw ordered turns that belong to a Session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnHistory {
    turns: Vec<ChatMessage>,
}

impl TurnHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_turns(turns: Vec<ChatMessage>) -> Self {
        Self { turns }
    }

    pub fn push(&mut self, turn: ChatMessage) {
        self.turns.push(turn);
    }

    pub fn as_turns(&self) -> &[ChatMessage] {
        &self.turns
    }
}

/// The model-visible context for one Run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelContext {
    messages: Vec<ChatMessage>,
    /// System prompt the agent built for this run, sent ahead of the
    /// conversation. `None` until the agent attaches one.
    system_prompt: Option<String>,
}

impl ModelContext {
    pub fn from_messages(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            system_prompt: None,
        }
    }

    /// Attach the system prompt to send ahead of the conversation.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// The system prompt to send ahead of the conversation, if set.
    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub(crate) fn push(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }
}

/// How a Session's [`TurnHistory`] becomes the [`ModelContext`] for one Run.
///
/// The strategy owns every decision about which turns reach the model: order,
/// ranking, pinning, summaries, and pruning. Keeping it behind one trait means
/// future context management swaps behavior in one place rather than spreading
/// across sessions, agents, and model adapters.
pub trait ContextStrategy: Send + Sync {
    /// Build the model-visible context for one Run from the raw Turn History.
    fn assemble(&self, history: &TurnHistory) -> ModelContext;
}

/// Forward every stored turn to the model in order.
///
/// This matches today's behavior exactly: the assembled messages are a verbatim
/// clone of the raw history, so no turn is dropped, ranked, or summarized. It is
/// the baseline every other strategy is measured against and the safe default
/// until ranking, pinning, or pruning lands.
#[derive(Clone, Debug, Default)]
pub struct FullForward;

impl FullForward {
    pub fn new() -> Self {
        Self
    }
}

impl ContextStrategy for FullForward {
    fn assemble(&self, history: &TurnHistory) -> ModelContext {
        ModelContext::from_messages(history.as_turns().to_vec())
    }
}

/// Builds the model-visible context for a Run from the Session's raw history.
///
/// Thin wrapper over a [`ContextStrategy`], defaulting to [`FullForward`] so the
/// assembled context is today's verbatim clone of the history.
#[derive(Clone, Debug, Default)]
pub struct ContextAssembler {
    strategy: FullForward,
}

impl ContextAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn assemble(&self, history: &TurnHistory) -> ModelContext {
        self.strategy.assemble(history)
    }
}

/// Share of a model's context window at which a [`TokenBudgetGuard`] warns.
///
/// 0.8 means a warning fires once the estimated context reaches 80% of the
/// window. Kept as a type rather than a bare `f64` so the threshold stays an
/// explicit decision at construction time.
#[derive(Clone, Copy, Debug)]
pub struct WarningThreshold(f64);

impl WarningThreshold {
    /// Warn when the estimate reaches 80% of the context window.
    pub const fn at_80_percent() -> Self {
        Self(0.8)
    }

    /// The threshold as a fraction in `0.0..=1.0`.
    pub fn as_ratio(self) -> f64 {
        self.0
    }
}

impl Default for WarningThreshold {
    fn default() -> Self {
        Self::at_80_percent()
    }
}

/// A pre-call estimate that crossed a [`TokenBudgetGuard`]'s threshold.
///
/// Carries everything the caller needs to surface a warning without re-counting:
/// the token count, the window it was measured against, and the estimate's
/// source/confidence so the message can note how much to trust it.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenBudgetWarning {
    /// Estimated tokens in the model-visible context.
    pub used: u64,
    /// The context window `used` was measured against.
    pub context_window: u64,
    /// How the estimate was produced (tokenizer vs. heuristic, and confidence).
    pub estimate: TokenEstimate,
}

impl TokenBudgetWarning {
    /// `used` as a fraction of `context_window`, in `0.0..=1.0`.
    pub fn ratio(&self) -> f64 {
        if self.context_window == 0 {
            return 0.0;
        }
        self.used as f64 / self.context_window as f64
    }
}

/// Estimates a [`ModelContext`]'s token size before a model call and reports
/// when it crosses a [`WarningThreshold`] of the model's context window.
///
/// This is the read-only half of context management: it does not truncate or
/// prune, only measures and warns. The estimate comes from
/// [`estimate_model_context`], so it uses whichever [`TextTokenCounter`] the
/// guard is configured with, falling back to the heuristic when no tokenizer is
/// available.
#[derive(Clone)]
pub struct TokenBudgetGuard {
    counter: Arc<dyn TextTokenCounter>,
    threshold: WarningThreshold,
}

/// A bound on a run's context size: the guard that measures it and the window
/// it is measured against. Bundled so the agent loop takes one budget argument
/// rather than a guard/window pair that must stay in sync.
pub struct TokenBudget<'a> {
    guard: &'a TokenBudgetGuard,
    context_window: Option<u64>,
}

impl<'a> TokenBudget<'a> {
    /// Build a budget from its guard and the model's context window.
    pub fn new(guard: &'a TokenBudgetGuard, context_window: Option<u64>) -> Self {
        Self {
            guard,
            context_window,
        }
    }

    /// Check the model's own `estimate` against this budget, returning a warning
    /// when it crosses the guard's threshold of the window.
    pub fn warn_if_over(&self, estimate: TokenEstimate) -> Option<TokenBudgetWarning> {
        self.guard.check_estimate(estimate, self.context_window)
    }
}

impl TokenBudgetGuard {
    /// Build a guard with the given counter that warns at 80% of the window.
    pub fn new(counter: Arc<dyn TextTokenCounter>) -> Self {
        Self {
            counter,
            threshold: WarningThreshold::default(),
        }
    }

    /// The fraction of the context window at which this guard warns.
    pub fn threshold(&self) -> WarningThreshold {
        self.threshold
    }

    /// Return a warning when `estimate` crosses `threshold` of `context_window`.
    /// Returns `None` when `context_window` is absent/zero or the estimate stays
    /// under the threshold.
    ///
    /// Use this when the caller already has an estimate (for example the model's
    /// own tokenizer-backed count); [`check`](Self::check) is the convenience that
    /// counts `context` with this guard's configured counter first.
    pub fn check_estimate(
        &self,
        estimate: TokenEstimate,
        context_window: Option<u64>,
    ) -> Option<TokenBudgetWarning> {
        let context_window = context_window.filter(|window| *window != 0)?;
        if (estimate.tokens as f64) >= self.threshold.as_ratio() * context_window as f64 {
            Some(TokenBudgetWarning {
                used: estimate.tokens,
                context_window,
                estimate,
            })
        } else {
            None
        }
    }

    /// Estimate `context` (with `tools`) using this guard's counter and return a
    /// warning when it crosses `threshold` of `context_window`. Returns `None`
    /// when `context_window` is absent/zero or the estimate stays under the
    /// threshold.
    pub fn check(
        &self,
        context: &ModelContext,
        tools: &[ToolDef],
        context_window: Option<u64>,
    ) -> Option<TokenBudgetWarning> {
        let estimate = estimate_model_context(context, tools, self.counter.as_ref());
        self.check_estimate(estimate, context_window)
    }
}
