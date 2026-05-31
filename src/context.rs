//! Context assembly for agent runs.
//!
//! Today this module deliberately preserves the existing behavior: every stored
//! turn is forwarded to the model in order. Naming that transformation gives
//! future context management one place to grow ranking, pinning, summaries,
//! citations, and pruning without spreading those decisions across sessions,
//! agents, and model adapters.

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

/// Builds the model-visible context for a Run from the Session's raw history.
#[derive(Clone, Debug, Default)]
pub struct ContextAssembler;

impl ContextAssembler {
    pub fn new() -> Self {
        Self
    }

    pub fn assemble(&self, history: &TurnHistory) -> ModelContext {
        ModelContext::from_messages(history.as_turns().to_vec())
    }
}
