use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mutation::{FileChangeSummary, FileDiffSummary, PatchApplyStatus};
use crate::permissions::ReviewDecision;

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
    /// The agent needs the operator's permission before it can run a tool
    /// call. Surfaced for both `bash` (`command` populated) and `edit_file`
    /// (`path` populated). Frontends respond by either rendering an
    /// interactive prompt (TUI) or by emitting a matching
    /// `ApprovalResponse` JSON line on stdin (NDJSON mode).
    ToolCallApprovalRequest {
        call_id: String,
        approval_id: String,
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        cwd: String,
        reason: String,
        available_decisions: Vec<ReviewDecision>,
    },
    /// A tool call was refused before execution. Stable `rule` ids let
    /// frontends localize or audit the rejection.
    ToolCallBlocked {
        call_id: String,
        tool: String,
        reason: String,
        rule: String,
    },
    TurnComplete {
        usage: TurnUsage,
    },
    /// The turn was aborted by the operator before it could produce a normal
    /// completion. Emitted exactly once per turn that ends in abort, in
    /// place of `TurnComplete`. `reason` is a short human-readable label
    /// ("user pressed esc", "approval modal abort", …) captured from the
    /// originating [`crate::AbortSignal`]. Durable so resume + replay can
    /// show the same outcome and so partial assistant text isn't treated as
    /// a finished answer.
    TurnAborted {
        reason: String,
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
            AgentEvent::ToolCallApprovalRequest { .. } => "tool_call_approval_request",
            AgentEvent::ToolCallBlocked { .. } => "tool_call_blocked",
            AgentEvent::TurnComplete { .. } => "turn_complete",
            AgentEvent::TurnAborted { .. } => "turn_aborted",
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_call_approval_request_wire_format() {
        let event = AgentEvent::ToolCallApprovalRequest {
            call_id: "c1".into(),
            approval_id: "a1".into(),
            tool: "bash".into(),
            command: Some(vec!["rm".into(), "-rf".into(), "build".into()]),
            path: None,
            cwd: "/ws".into(),
            reason: "dangerous_pattern".into(),
            available_decisions: vec![
                ReviewDecision::Approved,
                ReviewDecision::ApprovedForSession,
                ReviewDecision::Denied,
            ],
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "tool_call_approval_request",
                "call_id": "c1",
                "approval_id": "a1",
                "tool": "bash",
                "command": ["rm", "-rf", "build"],
                "cwd": "/ws",
                "reason": "dangerous_pattern",
                "available_decisions": ["approved", "approved_for_session", "denied"]
            })
        );
        assert_eq!(event.kind(), "tool_call_approval_request");
        assert!(event.is_durable());
    }

    #[test]
    fn tool_call_approval_request_skips_none_fields() {
        // edit_file approval uses `path` and omits `command`.
        let event = AgentEvent::ToolCallApprovalRequest {
            call_id: "c2".into(),
            approval_id: "a2".into(),
            tool: "edit_file".into(),
            command: None,
            path: Some("src/main.rs".into()),
            cwd: "/ws".into(),
            reason: "protected_metadata".into(),
            available_decisions: vec![ReviewDecision::Approved, ReviewDecision::Denied],
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("command").is_none(), "command should be skipped");
        assert_eq!(json["path"], "src/main.rs");
    }

    #[test]
    fn tool_call_blocked_wire_format() {
        let event = AgentEvent::ToolCallBlocked {
            call_id: "c3".into(),
            tool: "bash".into(),
            reason: "command sudo is never allowed".into(),
            rule: "unbypassable_dangerous".into(),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "tool_call_blocked",
                "call_id": "c3",
                "tool": "bash",
                "reason": "command sudo is never allowed",
                "rule": "unbypassable_dangerous"
            })
        );
        assert_eq!(event.kind(), "tool_call_blocked");
        assert!(event.is_durable());
    }

    #[test]
    fn turn_aborted_is_durable_and_serializes_with_reason() {
        let event = AgentEvent::TurnAborted {
            reason: "user pressed esc".into(),
        };
        assert_eq!(event.kind(), "turn_aborted");
        assert!(event.is_durable(), "TurnAborted must persist for replay");
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "turn_aborted",
                "reason": "user pressed esc"
            })
        );
    }

    #[test]
    fn existing_variants_kind_strings_unchanged() {
        // Guard against accidental rename of the wire-format discriminant.
        let cases = vec![
            (
                AgentEvent::UserMessage {
                    text: "x".into(),
                    display_text: None,
                    attachments: Vec::new(),
                },
                "user_message",
            ),
            (
                AgentEvent::AssistantMessageDelta { text: "x".into() },
                "assistant_message_delta",
            ),
            (
                AgentEvent::AssistantMessageDone { text: "x".into() },
                "assistant_message_done",
            ),
            (
                AgentEvent::ToolCallStarted {
                    call_id: "c".into(),
                    name: "n".into(),
                    arguments: serde_json::Value::Null,
                },
                "tool_call_started",
            ),
            (
                AgentEvent::ToolCallOutput {
                    call_id: "c".into(),
                    output: "o".into(),
                    is_error: false,
                },
                "tool_call_output",
            ),
            (
                AgentEvent::TurnComplete {
                    usage: TurnUsage::default(),
                },
                "turn_complete",
            ),
            (
                AgentEvent::Error {
                    message: "m".into(),
                },
                "error",
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(event.kind(), expected);
        }
    }
}
