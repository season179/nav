use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Normalized usage counters emitted at the end of each model turn.
///
/// Each field counts tokens for a single response; providers that do not
/// report a metric leave the corresponding field at `0`. Downstream consumers
/// (TUI status line, session store, billing) can rely on every variant of
/// [`AgentEvent::TurnComplete`] carrying these four fields populated.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_input_cached: u64,
    pub tokens_reasoning: u64,
}

/// Single, ordered events produced by [`crate::agent::run_agent`].
///
/// `UserMessage` records the exact model-facing prompt for replay and an
/// optional UI-facing display string. `AssistantMessageDelta` is the transient
/// stream chunk a renderer can paint incrementally; `AssistantMessageDone` is
/// fired once per assistant message with the coalesced final text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    UserMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_text: Option<String>,
    },
    AssistantMessageDelta {
        text: String,
    },
    AssistantMessageDone {
        text: String,
    },
    ToolCallStarted {
        call_id: String,
        name: String,
        arguments: Value,
    },
    ToolCallOutput {
        call_id: String,
        output: String,
        is_error: bool,
    },
    TurnComplete {
        usage: TurnUsage,
    },
    /// Emitted before sleeping during a retry of the streaming provider call.
    /// Surfaces transient failures so the TUI / session log can show that a
    /// hiccup was recovered from, not papered over.
    ProviderRetry {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        reason: String,
    },
    /// Emitted after the agent drops the oldest tool-call pair from the
    /// transcript in response to a `context_length_exceeded` error.
    /// `dropped_pairs` is the number of `function_call` + `function_call_output`
    /// pairs removed (currently always `1` — recovery is one-shot per session).
    ContextTrimmed {
        dropped_pairs: usize,
    },
    Error {
        message: String,
    },
}

impl AgentEvent {
    /// Returns the variant tag matched by the `serde(tag = "kind")` discriminant.
    /// Used as the `event.kind` column when persisting to the session store.
    pub fn kind(&self) -> &'static str {
        match self {
            AgentEvent::UserMessage { .. } => "user_message",
            AgentEvent::AssistantMessageDelta { .. } => "assistant_message_delta",
            AgentEvent::AssistantMessageDone { .. } => "assistant_message_done",
            AgentEvent::ToolCallStarted { .. } => "tool_call_started",
            AgentEvent::ToolCallOutput { .. } => "tool_call_output",
            AgentEvent::TurnComplete { .. } => "turn_complete",
            AgentEvent::ProviderRetry { .. } => "provider_retry",
            AgentEvent::ContextTrimmed { .. } => "context_trimmed",
            AgentEvent::Error { .. } => "error",
        }
    }

    /// `AssistantMessageDelta` is a stream chunk meant only for live rendering;
    /// `ProviderRetry` is a transient transport hint that adds no value on
    /// `--resume`. Every other variant is the canonical record of the
    /// conversation and must round-trip through the session log.
    pub fn is_durable(&self) -> bool {
        !matches!(
            self,
            AgentEvent::AssistantMessageDelta { .. } | AgentEvent::ProviderRetry { .. }
        )
    }
}
