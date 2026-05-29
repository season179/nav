use nav_types::{
    ApprovalId, EventId, FileChangeId, FileChangeKind, MessageId, PartId, RunId, SessionId,
    ToolCallId,
};
use serde::{Deserialize, Serialize};

pub const BACKEND_EVENT_TYPES: &[&str] = &[
    "session.created",
    "run.started",
    "model.text_delta",
    "model.reasoning_delta",
    "message.delta",
    "part.delta",
    "part.completed",
    "message.completed",
    "tool.call_requested",
    "tool.call_started",
    "tool.call_delta",
    "tool.output_delta",
    "tool.call_completed",
    "tool.call_failed",
    "tool.approval_requested",
    "file.changed",
    "run.completed",
    "run.cancelled",
    "run.failed",
    "provider.error",
    "error",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: EventId,
    pub session_id: SessionId,
    #[serde(flatten)]
    pub event: BackendEvent,
}

impl EventEnvelope {
    pub fn event_type(&self) -> &'static str {
        self.event.event_type()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendEvent {
    #[serde(rename = "session.created")]
    SessionCreated,
    #[serde(rename = "run.started")]
    RunStarted { run_id: RunId },
    #[serde(rename = "model.text_delta")]
    ModelTextDelta {
        run_id: RunId,
        message_id: MessageId,
        delta: String,
        metadata: ProviderEventMetadata,
    },
    #[serde(rename = "model.reasoning_delta")]
    ModelReasoningDelta {
        run_id: RunId,
        message_id: MessageId,
        delta: String,
        metadata: ProviderEventMetadata,
    },
    #[serde(rename = "message.delta")]
    MessageDelta {
        run_id: RunId,
        message_id: MessageId,
        text: String,
    },
    #[serde(rename = "part.delta")]
    PartDelta {
        turn_id: MessageId,
        part_id: PartId,
        field: String,
        delta: String,
    },
    #[serde(rename = "part.completed")]
    PartCompleted { turn_id: MessageId, part_id: PartId },
    #[serde(rename = "message.completed")]
    MessageCompleted {
        run_id: RunId,
        message_id: MessageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<ProviderEventMetadata>,
    },
    #[serde(rename = "tool.call_requested")]
    ToolCallRequested {
        run_id: RunId,
        tool_call_id: ToolCallId,
        name: String,
    },
    #[serde(rename = "tool.call_started")]
    ToolCallStarted {
        run_id: RunId,
        tool_call_id: ToolCallId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<ProviderEventMetadata>,
    },
    #[serde(rename = "tool.call_delta")]
    ToolCallDelta {
        run_id: RunId,
        tool_call_id: ToolCallId,
        arguments_delta: String,
        metadata: ProviderEventMetadata,
    },
    /// Live bash output progress. Deltas are raw in v1; consumers that need
    /// after-hook redaction should rely on `tool.call_completed.output`.
    #[serde(rename = "tool.output_delta")]
    ToolOutputDelta {
        run_id: RunId,
        tool_call_id: ToolCallId,
        stream: String,
        chunk: String,
    },
    #[serde(rename = "tool.call_completed")]
    ToolCallCompleted {
        run_id: RunId,
        tool_call_id: ToolCallId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        arguments: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_lossy: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<ProviderEventMetadata>,
    },
    #[serde(rename = "tool.call_failed")]
    ToolCallFailed {
        run_id: RunId,
        tool_call_id: ToolCallId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        error_message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_lossy: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<ProviderEventMetadata>,
    },
    #[serde(rename = "tool.approval_requested")]
    ToolApprovalRequested {
        run_id: RunId,
        tool_call_id: ToolCallId,
        approval_id: ApprovalId,
        tool_name: String,
        reason: String,
        arguments_summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        risk_class: Option<String>,
    },
    #[serde(rename = "file.changed")]
    FileChanged {
        file_change_id: FileChangeId,
        path: String,
        kind: FileChangeKind,
    },
    #[serde(rename = "run.completed")]
    RunCompleted {
        run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<ProviderEventMetadata>,
    },
    #[serde(rename = "run.cancelled")]
    RunCancelled { run_id: RunId },
    #[serde(rename = "run.failed")]
    RunFailed { run_id: RunId, message: String },
    #[serde(rename = "provider.error")]
    ProviderError {
        run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        metadata: ProviderEventMetadata,
    },
    #[serde(rename = "error")]
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEventMetadata {
    pub provider_id: String,
    pub configured_model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ProviderUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
}

impl BackendEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::SessionCreated => "session.created",
            Self::RunStarted { .. } => "run.started",
            Self::ModelTextDelta { .. } => "model.text_delta",
            Self::ModelReasoningDelta { .. } => "model.reasoning_delta",
            Self::MessageDelta { .. } => "message.delta",
            Self::PartDelta { .. } => "part.delta",
            Self::PartCompleted { .. } => "part.completed",
            Self::MessageCompleted { .. } => "message.completed",
            Self::ToolCallRequested { .. } => "tool.call_requested",
            Self::ToolCallStarted { .. } => "tool.call_started",
            Self::ToolCallDelta { .. } => "tool.call_delta",
            Self::ToolOutputDelta { .. } => "tool.output_delta",
            Self::ToolCallCompleted { .. } => "tool.call_completed",
            Self::ToolCallFailed { .. } => "tool.call_failed",
            Self::ToolApprovalRequested { .. } => "tool.approval_requested",
            Self::FileChanged { .. } => "file.changed",
            Self::RunCompleted { .. } => "run.completed",
            Self::RunCancelled { .. } => "run.cancelled",
            Self::RunFailed { .. } => "run.failed",
            Self::ProviderError { .. } => "provider.error",
            Self::Error { .. } => "error",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn advertised_event_types_match_backend_event_variants() {
        let advertised = BACKEND_EVENT_TYPES.iter().copied().collect::<BTreeSet<_>>();
        let emitted = backend_event_samples()
            .iter()
            .map(BackendEvent::event_type)
            .collect::<BTreeSet<_>>();

        assert_eq!(advertised, emitted);
    }

    #[test]
    fn file_changed_round_trips_every_kind() {
        for (kind, expected) in [
            (FileChangeKind::Created, "created"),
            (FileChangeKind::Modified, "modified"),
            (FileChangeKind::Deleted, "deleted"),
        ] {
            let event = BackendEvent::FileChanged {
                file_change_id: file_change_id(),
                path: "README.md".to_string(),
                kind,
            };

            let json = serde_json::to_value(&event).expect("file.changed should serialize");
            assert_eq!(json["type"], "file.changed");
            assert_eq!(json["kind"], expected);

            let decoded: BackendEvent =
                serde_json::from_value(json).expect("file.changed should deserialize");
            assert_eq!(decoded, event);
        }
    }

    fn backend_event_samples() -> Vec<BackendEvent> {
        vec![
            BackendEvent::SessionCreated,
            BackendEvent::RunStarted { run_id: run_id() },
            BackendEvent::ModelTextDelta {
                run_id: run_id(),
                message_id: message_id(),
                delta: "hello".to_string(),
                metadata: provider_metadata(),
            },
            BackendEvent::ModelReasoningDelta {
                run_id: run_id(),
                message_id: message_id(),
                delta: "thinking".to_string(),
                metadata: provider_metadata(),
            },
            BackendEvent::MessageDelta {
                run_id: run_id(),
                message_id: message_id(),
                text: "hello".to_string(),
            },
            BackendEvent::PartDelta {
                turn_id: message_id(),
                part_id: part_id(),
                field: "text".to_string(),
                delta: "hello".to_string(),
            },
            BackendEvent::PartCompleted {
                turn_id: message_id(),
                part_id: part_id(),
            },
            BackendEvent::MessageCompleted {
                run_id: run_id(),
                message_id: message_id(),
                finish_reason: Some("stop".to_string()),
                metadata: Some(provider_metadata()),
            },
            BackendEvent::ToolCallRequested {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                name: "read".to_string(),
            },
            BackendEvent::ToolCallStarted {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                name: Some("read".to_string()),
                metadata: Some(provider_metadata()),
            },
            BackendEvent::ToolCallDelta {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                arguments_delta: "{}".to_string(),
                metadata: provider_metadata(),
            },
            BackendEvent::ToolOutputDelta {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                stream: "stdout".to_string(),
                chunk: "hello\n".to_string(),
            },
            BackendEvent::ToolCallCompleted {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                name: Some("read".to_string()),
                arguments: "{}".to_string(),
                output: None,
                output_lossy: None,
                metadata: Some(provider_metadata()),
            },
            BackendEvent::ToolCallFailed {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                name: Some("read".to_string()),
                error_message: "file not found".to_string(),
                output: None,
                output_lossy: None,
                metadata: Some(provider_metadata()),
            },
            BackendEvent::ToolApprovalRequested {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                approval_id: approval_id(),
                tool_name: "write_file".to_string(),
                reason: "writes outside the current task focus".to_string(),
                arguments_summary: r#"{"path":"notes.md","content":"hello"}"#.to_string(),
                risk_class: Some("mutate".to_string()),
            },
            BackendEvent::FileChanged {
                file_change_id: file_change_id(),
                path: "README.md".to_string(),
                kind: FileChangeKind::Modified,
            },
            BackendEvent::RunCompleted {
                run_id: run_id(),
                metadata: Some(provider_metadata()),
            },
            BackendEvent::RunCancelled { run_id: run_id() },
            BackendEvent::RunFailed {
                run_id: run_id(),
                message: "failed".to_string(),
            },
            BackendEvent::ProviderError {
                run_id: run_id(),
                status: Some(500),
                message: "provider failed".to_string(),
                error_type: Some("server_error".to_string()),
                code: Some("server_error".to_string()),
                metadata: provider_metadata(),
            },
            BackendEvent::Error {
                message: "failed".to_string(),
            },
        ]
    }

    fn run_id() -> RunId {
        RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap()
    }

    fn message_id() -> MessageId {
        MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000002").unwrap()
    }

    fn tool_call_id() -> ToolCallId {
        ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000003").unwrap()
    }

    fn part_id() -> PartId {
        PartId::try_new("prt_0000018bcfe56800_0000000000000001").unwrap()
    }

    fn approval_id() -> ApprovalId {
        ApprovalId::try_new("019f2f6f-f178-7a72-9f28-000000000004").unwrap()
    }

    fn file_change_id() -> FileChangeId {
        FileChangeId::try_new("019f2f6f-f178-7a72-9f28-000000000005").unwrap()
    }

    fn provider_metadata() -> ProviderEventMetadata {
        ProviderEventMetadata {
            provider_id: "test-provider".to_string(),
            configured_model_id: "test-model".to_string(),
            provider_response_id: Some("response-1".to_string()),
            provider_model: Some("test-provider-model".to_string()),
            choice_index: Some(0),
            provider_tool_call_id: Some("tool-1".to_string()),
            usage: Some(ProviderUsage {
                prompt_tokens: Some(1),
                completion_tokens: Some(2),
                total_tokens: Some(3),
            }),
        }
    }
}
