use nav_types::{ApprovalId, EventId, FileChangeId, MessageId, RunId, SessionId, ToolCallId};
use serde::{Deserialize, Serialize};

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
    #[serde(rename = "message.delta")]
    MessageDelta {
        run_id: RunId,
        message_id: MessageId,
        text: String,
    },
    #[serde(rename = "message.completed")]
    MessageCompleted {
        run_id: RunId,
        message_id: MessageId,
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
    },
    #[serde(rename = "tool.call_completed")]
    ToolCallCompleted {
        run_id: RunId,
        tool_call_id: ToolCallId,
    },
    #[serde(rename = "tool.approval_requested")]
    ToolApprovalRequested {
        run_id: RunId,
        tool_call_id: ToolCallId,
        approval_id: ApprovalId,
    },
    #[serde(rename = "file.changed")]
    FileChanged {
        file_change_id: FileChangeId,
        path: String,
    },
    #[serde(rename = "run.completed")]
    RunCompleted { run_id: RunId },
    #[serde(rename = "run.cancelled")]
    RunCancelled { run_id: RunId },
    #[serde(rename = "run.failed")]
    RunFailed { run_id: RunId, message: String },
    #[serde(rename = "error")]
    Error { message: String },
}

impl BackendEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::SessionCreated => "session.created",
            Self::RunStarted { .. } => "run.started",
            Self::MessageDelta { .. } => "message.delta",
            Self::MessageCompleted { .. } => "message.completed",
            Self::ToolCallRequested { .. } => "tool.call_requested",
            Self::ToolCallStarted { .. } => "tool.call_started",
            Self::ToolCallCompleted { .. } => "tool.call_completed",
            Self::ToolApprovalRequested { .. } => "tool.approval_requested",
            Self::FileChanged { .. } => "file.changed",
            Self::RunCompleted { .. } => "run.completed",
            Self::RunCancelled { .. } => "run.cancelled",
            Self::RunFailed { .. } => "run.failed",
            Self::Error { .. } => "error",
        }
    }
}
