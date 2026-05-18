use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mutation::{FileChangeSummary, FileDiffSummary, PatchApplyStatus};

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
    FileChange {
        call_id: String,
        changes: Vec<FileChangeSummary>,
        status: PatchApplyStatus,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    TurnDiff {
        files: Vec<FileDiffSummary>,
        unified_diff: String,
        truncated: bool,
    },
    TurnComplete {
        usage: TurnUsage,
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
            AgentEvent::FileChange { .. } => "file_change",
            AgentEvent::TurnDiff { .. } => "turn_diff",
            AgentEvent::TurnComplete { .. } => "turn_complete",
            AgentEvent::Error { .. } => "error",
        }
    }

    /// `AssistantMessageDelta` is a stream chunk meant only for live rendering;
    /// every other variant is the canonical record of the conversation.
    pub fn is_durable(&self) -> bool {
        !matches!(self, AgentEvent::AssistantMessageDelta { .. })
    }
}
