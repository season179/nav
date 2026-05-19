use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mutation::{FileChangeSummary, FileDiffSummary, PatchApplyStatus};

/// A non-text input attached to a [`AgentEvent::UserMessage`]. Stored by path
/// (workspace-relative) — the bytes are loaded by the transport at request
/// time, so the session log doesn't bloat with base64. Resume rebuilds the
/// same input shape from the stored paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserAttachment {
    /// Pasted clipboard image or a recognized image-file paste. Path is
    /// always workspace-relative — the TUI relativizes / copies external
    /// paths into `<cwd>/.nav/clipboard/` before raising the event.
    Image { path: PathBuf },
}

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
        /// Non-text inputs (currently just clipboard / pasted images).
        /// `default` keeps old session-log rows readable after upgrade.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<UserAttachment>,
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
            AgentEvent::FileChange { .. } => "file_change",
            AgentEvent::TurnDiff { .. } => "turn_diff",
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
