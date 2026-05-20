use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

enum SubagentPhase {
    Started,
    Completed,
    Failed,
}

pub struct SubagentCell {
    phase: SubagentPhase,
    id: String,
    label: Option<String>,
    body: String,
}

impl SubagentCell {
    pub fn started(id: impl Into<String>, label: Option<String>, task: impl Into<String>) -> Self {
        Self {
            phase: SubagentPhase::Started,
            id: id.into(),
            label,
            body: task.into(),
        }
    }

    pub fn completed(
        id: impl Into<String>,
        label: Option<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            phase: SubagentPhase::Completed,
            id: id.into(),
            label,
            body: summary.into(),
        }
    }

    pub fn failed(
        id: impl Into<String>,
        label: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            phase: SubagentPhase::Failed,
            id: id.into(),
            label,
            body: message.into(),
        }
    }
}

impl HistoryCell for SubagentCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let name = self.display_name();
        let (kind, body) = match self.phase {
            SubagentPhase::Started => (
                TranscriptRowKind::SubagentStarted,
                format!("{name} started\n{}", self.body),
            ),
            SubagentPhase::Completed => (
                TranscriptRowKind::SubagentCompleted,
                format!("{name} completed\n{}", self.body),
            ),
            SubagentPhase::Failed => (
                TranscriptRowKind::SubagentFailed,
                format!("{name} failed\n{}", self.body),
            ),
        };
        TranscriptRow::new(kind, body).render(width)
    }
}

impl SubagentCell {
    fn display_name(&self) -> String {
        self.label
            .as_deref()
            .map(|label| format!("{label} ({})", self.id))
            .unwrap_or_else(|| self.id.clone())
    }
}
