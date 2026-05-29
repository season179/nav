//! In-memory session store used by the current server/tests.

use std::collections::HashMap;

use nav_types::SessionId;

use super::canonical::ModelTurn;

#[derive(Debug, Default)]
pub struct SessionStore {
    turns_by_session: HashMap<SessionId, Vec<ModelTurn>>,
}

impl SessionStore {
    pub fn create_session(&mut self, session_id: SessionId) {
        self.turns_by_session.entry(session_id).or_default();
    }

    pub fn append_turn(&mut self, session_id: &SessionId, turn: ModelTurn) {
        self.turns_by_session
            .entry(session_id.clone())
            .or_default()
            .push(turn);
    }

    pub fn turns(&self, session_id: &SessionId) -> Vec<ModelTurn> {
        self.turns_by_session
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }
}
