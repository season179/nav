//! Sessions, runs, messages, approvals, and long-lived task state.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

use nav_types::{ApprovalId, RunId, SessionId, ToolCallId};

#[derive(Debug, Default)]
pub struct SessionStore {
    turns_by_session: HashMap<SessionId, Vec<Turn>>,
}

impl SessionStore {
    pub fn create_session(&mut self, session_id: SessionId) {
        self.turns_by_session.entry(session_id).or_default();
    }

    pub fn append_turn(&mut self, session_id: &SessionId, turn: Turn) {
        self.turns_by_session
            .entry(session_id.clone())
            .or_default()
            .push(turn);
    }

    pub fn turns(&self, session_id: &SessionId) -> Vec<Turn> {
        self.turns_by_session
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: TurnRole,
    pub parts: Vec<TurnPart>,
}

impl Turn {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self::text(TurnRole::User, text)
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self::text(TurnRole::Assistant, text)
    }

    pub fn system_text(text: impl Into<String>) -> Self {
        Self::text(TurnRole::System, text)
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: TurnRole::Assistant,
            parts: tool_calls.into_iter().map(TurnPart::ToolCall).collect(),
        }
    }

    pub fn assistant_text_with_tool_calls(
        text: impl Into<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        let mut parts = vec![TurnPart::Text(text.into())];
        parts.extend(tool_calls.into_iter().map(TurnPart::ToolCall));
        Self {
            role: TurnRole::Assistant,
            parts,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: TurnRole::Tool,
            parts: vec![TurnPart::ToolResult {
                tool_call_id: tool_call_id.into(),
                content: content.into(),
            }],
        }
    }

    pub fn text_content(&self) -> String {
        self.parts
            .iter()
            .filter_map(TurnPart::text)
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.parts
            .iter()
            .filter_map(|part| match part {
                TurnPart::ToolCall(tool_call) => Some(tool_call.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn tool_call_id(&self) -> Option<&str> {
        self.parts.iter().find_map(|part| match part {
            TurnPart::ToolResult { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
    }

    fn text(role: TurnRole, text: impl Into<String>) -> Self {
        Self {
            role,
            parts: vec![TurnPart::Text(text.into())],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnPart {
    Text(String),
    ToolCall(ToolCall),
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

impl TurnPart {
    fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::ToolCall(_) => None,
            Self::ToolResult { content, .. } => Some(content),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub tool_call_id: Option<ToolCallId>,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingConfirmation {
    pub approval_id: ApprovalId,
    pub run_id: RunId,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub reason: String,
    pub arguments_summary: String,
    pub risk_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmationDecision {
    Approved,
    Rejected { reason: Option<String> },
}

#[derive(Debug, Default)]
pub struct PendingConfirmationRegistry {
    entries: HashMap<ApprovalId, PendingConfirmationEntry>,
}

impl PendingConfirmationRegistry {
    pub fn record(&mut self, pending: PendingConfirmation) -> Result<(), PendingConfirmationError> {
        self.insert(pending, None).map(|_| ())
    }

    pub fn register(
        &mut self,
        pending: PendingConfirmation,
    ) -> Result<PendingConfirmationReceiver, PendingConfirmationError> {
        let (sender, receiver) = mpsc::channel();
        self.insert(pending, Some(sender))?;

        Ok(PendingConfirmationReceiver { receiver })
    }

    pub fn resolve(
        &mut self,
        approval_id: &ApprovalId,
        decision: ConfirmationDecision,
    ) -> Result<PendingConfirmation, PendingConfirmationError> {
        let entry = self
            .entries
            .remove(approval_id)
            .ok_or_else(|| PendingConfirmationError::NotPending(approval_id.clone()))?;

        if let Some(sender) = entry.sender {
            let _ = sender.send(decision);
        }

        Ok(entry.pending)
    }

    pub fn clear_for_run(&mut self, run_id: &RunId) {
        self.entries
            .retain(|_, entry| &entry.pending.run_id != run_id);
    }

    fn insert(
        &mut self,
        pending: PendingConfirmation,
        sender: Option<Sender<ConfirmationDecision>>,
    ) -> Result<(), PendingConfirmationError> {
        if self.entries.contains_key(&pending.approval_id) {
            return Err(PendingConfirmationError::Duplicate(pending.approval_id));
        }

        self.entries.insert(
            pending.approval_id.clone(),
            PendingConfirmationEntry { pending, sender },
        );

        Ok(())
    }
}

#[derive(Debug)]
struct PendingConfirmationEntry {
    pending: PendingConfirmation,
    sender: Option<Sender<ConfirmationDecision>>,
}

#[derive(Debug)]
pub struct PendingConfirmationReceiver {
    receiver: Receiver<ConfirmationDecision>,
}

impl PendingConfirmationReceiver {
    pub fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<ConfirmationDecision, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingConfirmationError {
    Duplicate(ApprovalId),
    NotPending(ApprovalId),
}

impl fmt::Display for PendingConfirmationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(approval_id) => {
                write!(formatter, "approval `{approval_id}` is already pending")
            }
            Self::NotPending(approval_id) => {
                write!(formatter, "approval `{approval_id}` is not pending")
            }
        }
    }
}

impl Error for PendingConfirmationError {}
