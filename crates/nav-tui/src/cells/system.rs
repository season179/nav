use nav_core::ReviewDecision;
use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

pub struct NoticeCell {
    severity: NoticeSeverity,
    message: String,
}

enum NoticeSeverity {
    Info,
    Warning,
    Error,
}

impl NoticeCell {
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            severity: NoticeSeverity::Info,
            message: message.into(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: NoticeSeverity::Warning,
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: NoticeSeverity::Error,
            message: message.into(),
        }
    }
}

impl HistoryCell for NoticeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let kind = match self.severity {
            NoticeSeverity::Info => TranscriptRowKind::Notice,
            NoticeSeverity::Warning => TranscriptRowKind::TurnWarning,
            NoticeSeverity::Error => TranscriptRowKind::Error,
        };
        TranscriptRow::new(kind, self.message.as_str()).render(width)
    }
}

/// Labeled session/system notice (resume, export, name, etc.).
pub struct LabeledNoticeCell {
    label: String,
    message: String,
}

impl LabeledNoticeCell {
    pub fn new(label: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            message: message.into(),
        }
    }
}

impl HistoryCell for LabeledNoticeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        TranscriptRow::with_label(
            TranscriptRowKind::SessionNotice,
            self.label.as_str(),
            self.message.as_str(),
        )
        .render(width)
    }
}

pub struct ErrorCell {
    notice: NoticeCell,
}

impl ErrorCell {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            notice: NoticeCell::error(message),
        }
    }
}

impl HistoryCell for ErrorCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.notice.display_lines(width)
    }
}

pub struct ApprovalDecisionCell {
    decision: ReviewDecision,
}

impl ApprovalDecisionCell {
    pub fn new(decision: ReviewDecision) -> Self {
        Self { decision }
    }
}

impl HistoryCell for ApprovalDecisionCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let (kind, body) = approval_decision_policy(self.decision);
        TranscriptRow::new(kind, body).render(width)
    }
}

fn approval_decision_policy(decision: ReviewDecision) -> (TranscriptRowKind, &'static str) {
    match decision {
        ReviewDecision::Approved => (
            TranscriptRowKind::ApprovalApproved,
            "approved this tool call",
        ),
        ReviewDecision::ApprovedForSession => (
            TranscriptRowKind::ApprovalApproved,
            "approved matching tool calls for this session",
        ),
        ReviewDecision::Denied => (TranscriptRowKind::ApprovalDenied, "denied this tool call"),
        ReviewDecision::Abort => (TranscriptRowKind::ApprovalAborted, "aborted the turn"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn notices_do_not_render_as_labeled_error_rows() {
        let error = NoticeCell::error("network down");
        assert_eq!(line_text(&error.display_lines(80)[0]), "■ network down");

        let warning = NoticeCell::warning("retrying provider");
        assert_eq!(
            line_text(&warning.display_lines(80)[0]),
            "! retrying provider"
        );
    }

    #[test]
    fn approval_decisions_have_codex_style_audit_rows() {
        let approved = ApprovalDecisionCell::new(ReviewDecision::Approved);
        assert_eq!(
            line_text(&approved.display_lines(80)[0]),
            "✓ approved this tool call"
        );

        let denied = ApprovalDecisionCell::new(ReviewDecision::Denied);
        assert_eq!(
            line_text(&denied.display_lines(80)[0]),
            "! denied this tool call"
        );
    }
}
