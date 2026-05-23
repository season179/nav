use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_loop::control::PendingInputMode;
use crate::context::compaction::CompactionDetails;
use crate::git_checkpoint::{GitCheckpointAction, GitCheckpointStatus};
use crate::guardrails::ReviewDecision;
use crate::tool_registry::TruncationMeta;
use crate::verify::{FileChangeSummary, FileDiffSummary, PatchApplyStatus};

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
    /// Non-image file attached via `@file` mention. UTF-8 bodies are
    /// emitted as an `input_text` part; binaries surface as an inline note.
    File { path: PathBuf },
}

/// Provenance flag carried on every compaction lifecycle event so frontends can
/// tell the user whether nav compacted because they typed `/compact` or because
/// estimated token usage crossed the configured threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    Manual,
    Auto,
}

impl CompactionTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            CompactionTrigger::Manual => "manual",
            CompactionTrigger::Auto => "auto",
        }
    }
}

/// Why compaction was initiated. Mirrors codex's `CompactionReason` so
/// analytics consumers share a common vocabulary across harnesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionReason {
    /// User typed `/compact` (or a skill triggered an explicit compact).
    UserRequested,
    /// Token usage crossed the auto-compaction threshold between sampling
    /// iterations inside an ongoing turn.
    ContextLimit,
    /// A future path: downshifted to a smaller model with a shorter window.
    ModelDownshift,
}

impl CompactionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            CompactionReason::UserRequested => "user_requested",
            CompactionReason::ContextLimit => "context_limit",
            CompactionReason::ModelDownshift => "model_downshift",
        }
    }
}

/// When compaction ran relative to the surrounding turn lifecycle. Named
/// `CompactionAnalyticsPhase` (not `CompactionPhase`) to avoid colliding
/// with the TUI's rendering-phase enum in `nav-tui::cells::compaction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionAnalyticsPhase {
    /// Compaction ran as its own isolated turn (manual `/compact`).
    StandaloneTurn,
    /// Compaction ran between sampling iterations inside an ongoing user
    /// turn — codex's "mid-turn" path, gated on a tool-call follow-up
    /// being needed when the token threshold is crossed.
    MidTurn,
}

impl CompactionAnalyticsPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            CompactionAnalyticsPhase::StandaloneTurn => "standalone_turn",
            CompactionAnalyticsPhase::MidTurn => "mid_turn",
        }
    }
}

/// Outcome of a compaction attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStatus {
    Completed,
    /// Reserved for future use: compaction aborted mid-flight by a
    /// pre-emption signal (e.g. user cancels the turn while compacting).
    Interrupted,
    Failed,
}

impl CompactionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CompactionStatus::Completed => "completed",
            CompactionStatus::Interrupted => "interrupted",
            CompactionStatus::Failed => "failed",
        }
    }
}

/// Structured analytics event emitted exactly once per compaction attempt.
/// Routed to `tracing::info!(target: "nav.compaction", …)` — this is a
/// telemetry-only event and must NOT appear on the user-facing
/// [`AgentEvent`] stream. nav does not currently have a dedicated telemetry
/// sink, so structured tracing is the lightest-weight option that avoids
/// coupling the analytics surface to the protocol event bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionAnalyticsEvent {
    pub trigger: CompactionTrigger,
    pub reason: CompactionReason,
    pub phase: CompactionAnalyticsPhase,
    pub status: CompactionStatus,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub duration_ms: u64,
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

/// Single, ordered events produced by [`crate::agent_loop::run_agent`].
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
        /// Non-text inputs. Empty attachment lists are omitted from stored
        /// JSON, so deserialization supplies an empty list by default.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<UserAttachment>,
    },
    AssistantMessageDelta {
        text: String,
    },
    AssistantMessageDone {
        text: String,
    },
    /// Streaming reasoning / chain-of-thought text delta. Emitted by
    /// reasoning-capable models (o-series, etc.) alongside the regular
    /// assistant message. Transient — not persisted to the session log,
    /// matching `AssistantMessageDelta` semantics.
    ReasoningDelta {
        text: String,
    },
    /// Transient reasoning text emitted once per reasoning item when the
    /// provider finishes it. Carries the coalesced summary text so the
    /// TUI can render a collapsible `ReasoningCell` in scrollback. Not
    /// persisted to the session log — the encrypted handle in
    /// `ResponseContinuation` is the durable record.
    ReasoningDone {
        text: String,
    },
    /// Provider response items nav needs to replay a `store: false` tool turn
    /// across separate `run_agent` invocations. Carries the model's
    /// `function_call` items verbatim, plus any reasoning items reduced to the
    /// `id` + `encrypted_content` continuation handle. Hidden plaintext
    /// reasoning (`summary`, `content`) and assistant `message` items are
    /// excluded here; the durable assistant message lives in
    /// `AssistantMessageDone`.
    ResponseContinuation {
        items: Vec<Value>,
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
        /// Truncation/spillover metadata. Optional in serialized form so
        /// older session logs (which never carried this field) round-trip
        /// unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        truncation: Option<TruncationMeta>,
    },
    SubagentStarted {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        task: String,
    },
    SubagentCompleted {
        id: String,
        summary: String,
    },
    SubagentFailed {
        id: String,
        message: String,
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
    /// A local git checkpoint/stash/restore operation completed. These
    /// events are not model-facing; they are durable UI/audit rows so a user
    /// can later see which reversible snapshots existed around a turn.
    GitCheckpoint {
        action: GitCheckpointAction,
        status: GitCheckpointStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stash_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stash_oid: Option<String>,
        message: String,
    },
    /// The agent needs the operator's permission before it can run a tool
    /// call. Surfaced for both `bash` (`command` populated) and `edit_file`
    /// (`path` populated). Frontends respond by either rendering an
    /// interactive prompt (TUI) or by emitting a matching
    /// approval response JSON line on stdin (headless modes).
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
    /// The operator answered a pending approval request. This is durable UI
    /// audit data; replay ignores it for model context, while the session
    /// approval side table mirrors the same decision for queries.
    ToolCallApprovalDecision {
        approval_id: String,
        decision: ReviewDecision,
    },
    /// A tool call was refused before execution. Stable `rule` ids let
    /// frontends localize or audit the rejection.
    ToolCallBlocked {
        call_id: String,
        tool: String,
        reason: String,
        rule: String,
    },
    PendingInputQueued {
        id: String,
        mode: PendingInputMode,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<UserAttachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skill_name: Option<String>,
    },
    PendingInputEdited {
        id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<UserAttachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skill_name: Option<String>,
    },
    PendingInputRemoved {
        id: String,
    },
    PendingInputCleared {
        ids: Vec<String>,
    },
    PendingInputDequeued {
        id: String,
        mode: PendingInputMode,
    },
    TurnComplete {
        usage: TurnUsage,
    },
    TurnAborted {
        turn_id: String,
        reason: String,
    },
    /// Recorded after the user rewinds the session to an earlier
    /// `user_message`. The original message at `target_seq` and every event
    /// that followed it have been removed from the event log; this audit
    /// row takes their place so the durable transcript still shows where
    /// the rewind happened. Replay drops the event — it carries no
    /// model-visible content.
    SessionRewound {
        target_seq: u64,
        removed_events: usize,
        preview: String,
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
    /// Emitted when the tool-call count within a single user turn crosses a
    /// multiple of `soft_budget`. nav also injects a model-visible steering
    /// message so the agent is nudged to produce a deliverable or justify
    /// continued exploration. `tool_calls` is the running count for the
    /// current user turn; `soft_budget` is the configured threshold.
    ToolBudgetWarning {
        tool_calls: usize,
        soft_budget: usize,
    },
    /// A compaction turn is about to start. `tokens_before` is the lifetime
    /// cumulative `tokens_input` recorded against this session at the
    /// moment of compaction; `0` if no session totals were available. Auto
    /// compaction decisions read this back as a baseline so post-checkpoint
    /// usage can be measured separately from lifetime totals. Frontends
    /// should freeze the composer / queue new user prompts until the
    /// matching `CompactionCompleted` or `CompactionFailed` arrives.
    CompactionStarted {
        trigger: CompactionTrigger,
        tokens_before: u64,
    },
    /// The compaction turn finished successfully. `summary` is the persisted
    /// handoff summary; subsequent turns replay from this checkpoint instead
    /// of the full pre-compaction transcript. `replaced_events` reports how
    /// many Responses-API input items (messages, function calls, function
    /// outputs) are now hidden behind the summary in the model-visible
    /// history. `tokens_before` matches the value on the paired
    /// `CompactionStarted` — lifetime cumulative `tokens_input` at
    /// compaction time, used as the baseline for the next auto-compaction
    /// decision. Visible scrollback is preserved separately by the session
    /// event log.
    CompactionCompleted {
        trigger: CompactionTrigger,
        summary: String,
        replaced_events: usize,
        tokens_before: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<CompactionDetails>,
    },
    /// Compaction failed; `message` carries the underlying error. The session
    /// is still using the pre-compaction transcript — the next turn replays
    /// the same history as before.
    CompactionFailed {
        trigger: CompactionTrigger,
        message: String,
    },
    /// A hook from an extension is about to run. Persisted for audit;
    /// the TUI does not render an in-progress indicator (see
    /// [`HookCompleted`] for the visible cell).
    HookStarted {
        name: String,
        /// The hook trigger (e.g. `"pre_turn"`), matching the extension
        /// manifest's `"event"` field.
        event_type: String,
    },
    /// A hook from an extension finished executing. The TUI uses duration
    /// and output to decide visibility (see CELL-04 HookCell spec).
    HookCompleted {
        name: String,
        /// The hook trigger (e.g. `"pre_turn"`), matching the extension
        /// manifest's `"event"` field.
        event_type: String,
        duration_ms: u64,
        stdout: String,
        stderr: String,
        success: bool,
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
            AgentEvent::ReasoningDelta { .. } => "reasoning_delta",
            AgentEvent::ReasoningDone { .. } => "reasoning_done",
            AgentEvent::ResponseContinuation { .. } => "response_continuation",
            AgentEvent::ToolCallStarted { .. } => "tool_call_started",
            AgentEvent::ToolCallOutput { .. } => "tool_call_output",
            AgentEvent::SubagentStarted { .. } => "subagent_started",
            AgentEvent::SubagentCompleted { .. } => "subagent_completed",
            AgentEvent::SubagentFailed { .. } => "subagent_failed",
            AgentEvent::FileChange { .. } => "file_change",
            AgentEvent::TurnDiff { .. } => "turn_diff",
            AgentEvent::GitCheckpoint { .. } => "git_checkpoint",
            AgentEvent::ToolCallApprovalRequest { .. } => "tool_call_approval_request",
            AgentEvent::ToolCallApprovalDecision { .. } => "tool_call_approval_decision",
            AgentEvent::ToolCallBlocked { .. } => "tool_call_blocked",
            AgentEvent::PendingInputQueued { .. } => "pending_input_queued",
            AgentEvent::PendingInputEdited { .. } => "pending_input_edited",
            AgentEvent::PendingInputRemoved { .. } => "pending_input_removed",
            AgentEvent::PendingInputCleared { .. } => "pending_input_cleared",
            AgentEvent::PendingInputDequeued { .. } => "pending_input_dequeued",
            AgentEvent::TurnComplete { .. } => "turn_complete",
            AgentEvent::TurnAborted { .. } => "turn_aborted",
            AgentEvent::SessionRewound { .. } => "session_rewound",
            AgentEvent::ProviderRetry { .. } => "provider_retry",
            AgentEvent::ContextTrimmed { .. } => "context_trimmed",
            AgentEvent::ToolBudgetWarning { .. } => "tool_budget_warning",
            AgentEvent::CompactionStarted { .. } => "compaction_started",
            AgentEvent::CompactionCompleted { .. } => "compaction_completed",
            AgentEvent::CompactionFailed { .. } => "compaction_failed",
            AgentEvent::HookStarted { .. } => "hook_started",
            AgentEvent::HookCompleted { .. } => "hook_completed",
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
            AgentEvent::AssistantMessageDelta { .. }
            | AgentEvent::ReasoningDelta { .. }
            | AgentEvent::ReasoningDone { .. }
            | AgentEvent::ProviderRetry { .. }
        )
    }
}

impl From<crate::git_checkpoint::GitCheckpointOutcome> for AgentEvent {
    fn from(outcome: crate::git_checkpoint::GitCheckpointOutcome) -> Self {
        AgentEvent::GitCheckpoint {
            action: outcome.action,
            status: outcome.status,
            stash_ref: outcome.stash_ref,
            stash_oid: outcome.stash_oid,
            message: outcome.message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::control::PendingInputMode;
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
    fn tool_call_approval_decision_wire_format() {
        let event = AgentEvent::ToolCallApprovalDecision {
            approval_id: "a1".into(),
            decision: ReviewDecision::ApprovedForSession,
        };

        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "tool_call_approval_decision",
                "approval_id": "a1",
                "decision": "approved_for_session",
            })
        );
        assert_eq!(event.kind(), "tool_call_approval_decision");
        assert!(event.is_durable());
    }

    #[test]
    fn pending_input_events_have_stable_wire_format() {
        let event = AgentEvent::PendingInputQueued {
            id: "pending-1".into(),
            mode: PendingInputMode::FollowUp,
            text: "next task".into(),
            display_text: None,
            attachments: vec![UserAttachment::Image {
                path: "screens/one.png".into(),
            }],
            skill_name: Some("tdd".into()),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "pending_input_queued",
                "id": "pending-1",
                "mode": "follow_up",
                "text": "next task",
                "attachments": [{"kind": "image", "path": "screens/one.png"}],
                "skill_name": "tdd"
            })
        );
        assert_eq!(event.kind(), "pending_input_queued");
        assert!(event.is_durable());

        assert_eq!(
            serde_json::to_value(AgentEvent::PendingInputDequeued {
                id: "pending-1".into(),
                mode: PendingInputMode::FollowUp,
            })
            .unwrap(),
            json!({
                "kind": "pending_input_dequeued",
                "id": "pending-1",
                "mode": "follow_up"
            })
        );

        assert_eq!(
            serde_json::to_value(AgentEvent::PendingInputEdited {
                id: "pending-1".into(),
                text: "clearer next task".into(),
                display_text: Some("clearer request".into()),
                attachments: Vec::new(),
                skill_name: None,
            })
            .unwrap(),
            json!({
                "kind": "pending_input_edited",
                "id": "pending-1",
                "text": "clearer next task",
                "display_text": "clearer request"
            })
        );
        assert_eq!(
            serde_json::to_value(AgentEvent::PendingInputRemoved {
                id: "pending-1".into(),
            })
            .unwrap(),
            json!({
                "kind": "pending_input_removed",
                "id": "pending-1"
            })
        );
        assert_eq!(
            serde_json::to_value(AgentEvent::PendingInputCleared {
                ids: vec!["pending-1".into(), "pending-2".into()],
            })
            .unwrap(),
            json!({
                "kind": "pending_input_cleared",
                "ids": ["pending-1", "pending-2"]
            })
        );
    }

    #[test]
    fn subagent_events_have_stable_wire_format() {
        let started = AgentEvent::SubagentStarted {
            id: "call_1".into(),
            label: Some("reviewer".into()),
            task: "check the diff".into(),
        };
        assert_eq!(
            serde_json::to_value(&started).unwrap(),
            json!({
                "kind": "subagent_started",
                "id": "call_1",
                "label": "reviewer",
                "task": "check the diff"
            })
        );
        assert_eq!(started.kind(), "subagent_started");
        assert!(started.is_durable());

        let completed = AgentEvent::SubagentCompleted {
            id: "call_1".into(),
            summary: "clean".into(),
        };
        assert_eq!(
            serde_json::to_value(&completed).unwrap(),
            json!({
                "kind": "subagent_completed",
                "id": "call_1",
                "summary": "clean"
            })
        );
        assert_eq!(completed.kind(), "subagent_completed");
        assert!(completed.is_durable());
    }

    #[test]
    fn user_attachment_file_variant_round_trips() {
        let attach = UserAttachment::File {
            path: "src/main.rs".into(),
        };
        let json = serde_json::to_value(&attach).unwrap();
        assert_eq!(json, json!({"kind": "file", "path": "src/main.rs"}));
        let parsed: UserAttachment = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, attach);
    }

    #[test]
    fn user_attachment_image_variant_round_trips() {
        let attach = UserAttachment::Image {
            path: "a.png".into(),
        };
        let json = serde_json::to_value(&attach).unwrap();
        assert_eq!(json, json!({"kind": "image", "path": "a.png"}));
        let parsed: UserAttachment = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, attach);
    }

    #[test]
    fn abort_event_is_durable_and_separate_from_normal_completion() {
        let event = AgentEvent::TurnAborted {
            turn_id: "turn-1".into(),
            reason: "user interrupt".into(),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "turn_aborted",
                "turn_id": "turn-1",
                "reason": "user interrupt"
            })
        );
        assert_eq!(event.kind(), "turn_aborted");
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
    fn tool_call_output_truncation_metadata_wire_format() {
        use crate::tool_registry::{TruncationKind, TruncationMeta};

        // Spillover case: serialized form nests the metadata under
        // `truncation` so durable events let consumers locate the full
        // bash output on disk and the model can call `expand_artifact`.
        let event = AgentEvent::ToolCallOutput {
            call_id: "c1".into(),
            output: "head...\n[Full output: /tmp/x.log]\n[Artifact: bash-x — call expand_artifact with artifact_id=\"bash-x\" to read the raw output]".into(),
            is_error: false,
            truncation: Some(TruncationMeta {
                truncated_by: TruncationKind::BashSpill,
                full_output_path: Some("/tmp/x.log".into()),
                artifact_id: Some("bash-x".into()),
            }),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "tool_call_output",
                "call_id": "c1",
                "output": "head...\n[Full output: /tmp/x.log]\n[Artifact: bash-x — call expand_artifact with artifact_id=\"bash-x\" to read the raw output]",
                "is_error": false,
                "truncation": {
                    "truncated_by": "bash_spill",
                    "full_output_path": "/tmp/x.log",
                    "artifact_id": "bash-x",
                },
            })
        );

        // Untruncated case omits the optional field entirely.
        let plain = AgentEvent::ToolCallOutput {
            call_id: "c2".into(),
            output: "ok".into(),
            is_error: false,
            truncation: None,
        };
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("truncation").is_none());

        // Round-trip a legacy payload that omits `truncation`: deserializes
        // to `None` so existing session logs still replay.
        let legacy: AgentEvent = serde_json::from_value(json!({
            "kind": "tool_call_output",
            "call_id": "c3",
            "output": "legacy",
            "is_error": false,
        }))
        .unwrap();
        match legacy {
            AgentEvent::ToolCallOutput { truncation, .. } => {
                assert!(truncation.is_none());
            }
            other => panic!("expected ToolCallOutput, got {other:?}"),
        }
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
    fn tool_budget_warning_wire_format() {
        let event = AgentEvent::ToolBudgetWarning {
            tool_calls: 25,
            soft_budget: 25,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "tool_budget_warning",
                "tool_calls": 25,
                "soft_budget": 25,
            })
        );
        assert_eq!(event.kind(), "tool_budget_warning");
        assert!(event.is_durable());
    }

    #[test]
    fn compaction_started_wire_format() {
        let event = AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Manual,
            tokens_before: 12_345,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "compaction_started",
                "trigger": "manual",
                "tokens_before": 12345
            })
        );
        assert_eq!(event.kind(), "compaction_started");
        assert!(event.is_durable());
    }

    #[test]
    fn compaction_completed_wire_format() {
        let event = AgentEvent::CompactionCompleted {
            trigger: CompactionTrigger::Auto,
            summary: "did things".into(),
            replaced_events: 8,
            tokens_before: 200_000,
            details: Some(CompactionDetails {
                read_files: vec!["src/lib.rs".into()],
                modified_files: vec!["src/main.rs".into()],
            }),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "compaction_completed",
                "trigger": "auto",
                "summary": "did things",
                "replaced_events": 8,
                "tokens_before": 200000,
                "details": {
                    "read_files": ["src/lib.rs"],
                    "modified_files": ["src/main.rs"]
                }
            })
        );
        assert_eq!(event.kind(), "compaction_completed");
        assert!(event.is_durable());
    }

    #[test]
    fn compaction_failed_wire_format() {
        let event = AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            message: "transport closed".into(),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "compaction_failed",
                "trigger": "manual",
                "message": "transport closed"
            })
        );
        assert_eq!(event.kind(), "compaction_failed");
        assert!(event.is_durable());
    }

    #[test]
    fn git_checkpoint_wire_format() {
        let event = AgentEvent::GitCheckpoint {
            action: GitCheckpointAction::Checkpoint,
            status: GitCheckpointStatus::Created,
            stash_ref: Some("stash@{0}".into()),
            stash_oid: Some("abc123".into()),
            message: "nav checkpoint 01ABCDEF: before turn".into(),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "kind": "git_checkpoint",
                "action": "checkpoint",
                "status": "created",
                "stash_ref": "stash@{0}",
                "stash_oid": "abc123",
                "message": "nav checkpoint 01ABCDEF: before turn"
            })
        );
        assert_eq!(event.kind(), "git_checkpoint");
        assert!(event.is_durable());
    }

    #[test]
    fn hook_events_have_stable_wire_format() {
        let started = AgentEvent::HookStarted {
            name: "pre_turn".into(),
            event_type: "pre_turn".into(),
        };
        assert_eq!(
            serde_json::to_value(&started).unwrap(),
            json!({
                "kind": "hook_started",
                "name": "pre_turn",
                "event_type": "pre_turn"
            })
        );
        assert_eq!(started.kind(), "hook_started");
        assert!(started.is_durable());

        let completed = AgentEvent::HookCompleted {
            name: "pre_commit".into(),
            event_type: "pre_commit".into(),
            duration_ms: 350,
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        };
        assert_eq!(
            serde_json::to_value(&completed).unwrap(),
            json!({
                "kind": "hook_completed",
                "name": "pre_commit",
                "event_type": "pre_commit",
                "duration_ms": 350,
                "stdout": "",
                "stderr": "",
                "success": true
            })
        );
        assert_eq!(completed.kind(), "hook_completed");
        assert!(completed.is_durable());

        // Round-trip deserialization.
        let json = serde_json::to_value(&completed).unwrap();
        let rt: AgentEvent = serde_json::from_value(json).unwrap();
        assert_eq!(rt.kind(), "hook_completed");

        // Failed hook with output round-trips.
        let failed = AgentEvent::HookCompleted {
            name: "lint".into(),
            event_type: "pre_commit".into(),
            duration_ms: 1200,
            stdout: "3 warnings".into(),
            stderr: "type mismatch".into(),
            success: false,
        };
        let json = serde_json::to_value(&failed).unwrap();
        let rt: AgentEvent = serde_json::from_value(json).unwrap();
        assert_eq!(rt.kind(), "hook_completed");
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
                    truncation: None,
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
            (
                AgentEvent::ReasoningDelta { text: "x".into() },
                "reasoning_delta",
            ),
            (
                AgentEvent::ReasoningDone { text: "x".into() },
                "reasoning_done",
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(event.kind(), expected);
        }
    }

    #[test]
    fn reasoning_events_are_transient() {
        assert!(
            !AgentEvent::ReasoningDelta { text: "x".into() }.is_durable(),
            "ReasoningDelta must be transient so it does not bloat the session log"
        );
        assert!(
            !AgentEvent::ReasoningDone { text: "x".into() }.is_durable(),
            "ReasoningDone must be transient — the encrypted handle in ResponseContinuation \n             is the durable record; the plaintext summary is TUI-only"
        );
    }
}
