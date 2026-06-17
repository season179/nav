//! Context assembly for agent runs.
//!
//! Today this module deliberately preserves the existing behavior: every stored
//! turn is forwarded to the model in order. Naming that transformation as a
//! [`ContextStrategy`] gives future context management one seam to grow ranking,
//! pinning, summaries, citations, and pruning without spreading those decisions
//! across sessions, agents, and model adapters.

use crate::model::ChatMessage;

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
