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

    pub fn text_content(&self) -> String {
        self.parts
            .iter()
            .map(TurnPart::text)
            .collect::<Vec<_>>()
            .join("")
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
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnPart {
    Text(String),
}

impl TurnPart {
    fn text(&self) -> &str {
        match self {
            Self::Text(text) => text,
        }
    }
}
