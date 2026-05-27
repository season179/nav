//! Sessions, runs, messages, approvals, and long-lived task state.

use std::collections::HashMap;

use nav_types::SessionId;

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
    pub name: String,
    pub arguments: String,
}
