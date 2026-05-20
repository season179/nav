use nav_core::PendingInputMode;
use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

pub struct PendingInputCell {
    action: PendingInputAction,
    id: Option<String>,
    mode: Option<PendingInputMode>,
    text: Option<String>,
    skill_name: Option<String>,
    ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingInputAction {
    Queued,
    Edited,
    Removed,
    Cleared,
    Dequeued,
}

impl PendingInputCell {
    pub fn queued(
        id: impl Into<String>,
        mode: PendingInputMode,
        text: impl Into<String>,
        skill_name: Option<String>,
    ) -> Self {
        Self {
            action: PendingInputAction::Queued,
            id: Some(id.into()),
            mode: Some(mode),
            text: Some(text.into()),
            skill_name,
            ids: Vec::new(),
        }
    }

    pub fn edited(
        id: impl Into<String>,
        text: impl Into<String>,
        skill_name: Option<String>,
    ) -> Self {
        Self {
            action: PendingInputAction::Edited,
            id: Some(id.into()),
            mode: None,
            text: Some(text.into()),
            skill_name,
            ids: Vec::new(),
        }
    }

    pub fn removed(id: impl Into<String>) -> Self {
        Self {
            action: PendingInputAction::Removed,
            id: Some(id.into()),
            mode: None,
            text: None,
            skill_name: None,
            ids: Vec::new(),
        }
    }

    pub fn cleared(ids: Vec<String>) -> Self {
        Self {
            action: PendingInputAction::Cleared,
            id: None,
            mode: None,
            text: None,
            skill_name: None,
            ids,
        }
    }

    pub fn dequeued(id: impl Into<String>, mode: PendingInputMode) -> Self {
        Self {
            action: PendingInputAction::Dequeued,
            id: Some(id.into()),
            mode: Some(mode),
            text: None,
            skill_name: None,
            ids: Vec::new(),
        }
    }
}

impl HistoryCell for PendingInputCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let kind = match self.action {
            PendingInputAction::Queued => TranscriptRowKind::PendingQueued,
            PendingInputAction::Edited => TranscriptRowKind::PendingEdited,
            PendingInputAction::Removed => TranscriptRowKind::PendingRemoved,
            PendingInputAction::Cleared => TranscriptRowKind::PendingCleared,
            PendingInputAction::Dequeued => TranscriptRowKind::PendingDequeued,
        };
        TranscriptRow::new(kind, pending_input_body(self)).render(width)
    }
}

fn pending_input_body(cell: &PendingInputCell) -> String {
    match cell.action {
        PendingInputAction::Queued => {
            let title = format!("{} {}", pending_id(cell), mode_label(cell.mode));
            pending_body_with_details(title, cell.text.as_deref(), cell.skill_name.as_deref())
        }
        PendingInputAction::Edited => pending_body_with_details(
            pending_id(cell).to_string(),
            cell.text.as_deref(),
            cell.skill_name.as_deref(),
        ),
        PendingInputAction::Removed => pending_id(cell).to_string(),
        PendingInputAction::Cleared => cleared_body(&cell.ids),
        PendingInputAction::Dequeued => {
            format!("{} {}", pending_id(cell), mode_label(cell.mode))
        }
    }
}

fn pending_id(cell: &PendingInputCell) -> &str {
    cell.id.as_deref().unwrap_or("<pending>")
}

fn pending_body_with_details(
    title: String,
    text: Option<&str>,
    skill_name: Option<&str>,
) -> String {
    let mut parts = vec![title];
    if let Some(text) = text {
        parts.push(text.to_string());
    }
    if let Some(skill) = skill_name {
        parts.push(format!("{skill} skill"));
    }
    parts.join("\n")
}

fn cleared_body(ids: &[String]) -> String {
    if ids.is_empty() {
        "pending queue empty".to_string()
    } else {
        format!("cleared {}", ids.join(", "))
    }
}

fn mode_label(mode: Option<PendingInputMode>) -> &'static str {
    match mode {
        Some(PendingInputMode::FollowUp) => "follow-up",
        Some(PendingInputMode::Steering) => "steering",
        None => "pending",
    }
}

pub struct TurnAbortedCell {
    turn_id: String,
    reason: String,
}

impl TurnAbortedCell {
    pub fn new(turn_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            turn_id: turn_id.into(),
            reason: reason.into(),
        }
    }
}

impl HistoryCell for TurnAbortedCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        TranscriptRow::new(
            TranscriptRowKind::TurnAborted,
            format!("{} {}", self.turn_id, self.reason),
        )
        .render(width)
    }
}
