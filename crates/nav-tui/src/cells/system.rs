use nav_core::{ReviewDecision, TurnUsage};
use ratatui::style::{Color, Style};
use ratatui::text::Line;

use crate::history::HistoryCell;

use super::row::{TranscriptRow, TranscriptRowKind};

pub struct NoticeCell {
    severity: NoticeSeverity,
    message: String,
}

enum NoticeSeverity {
    Warning,
    Error,
}

impl NoticeCell {
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
            NoticeSeverity::Warning => TranscriptRowKind::TurnWarning,
            NoticeSeverity::Error => TranscriptRowKind::Error,
        };
        TranscriptRow::new(kind, self.message.as_str()).render(width)
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

pub struct TurnSeparatorCell {
    usage: TurnUsage,
}

impl TurnSeparatorCell {
    pub fn new(usage: TurnUsage) -> Self {
        Self { usage }
    }
}

impl HistoryCell for TurnSeparatorCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let text = separator_text(width, usage_summary(&self.usage));
        let mut line = Line::from(text);
        line.style = Style::default().fg(Color::DarkGray);
        vec![line, Line::from(String::new())]
    }
}

fn separator_text(width: u16, summary: String) -> String {
    let width = width as usize;
    if width == 0 {
        return String::new();
    }
    let prefix = format!("─ {summary} ");
    let prefix_width = prefix.chars().count();
    if prefix_width >= width {
        return prefix.chars().take(width).collect();
    }
    format!("{prefix}{}", "─".repeat(width - prefix_width))
}

fn usage_summary(usage: &TurnUsage) -> String {
    let mut parts = Vec::new();
    if usage.tokens_input > 0 {
        parts.push(format!("{} in", usage.tokens_input));
    }
    if usage.tokens_input_cached > 0 {
        parts.push(format!("{} cached", usage.tokens_input_cached));
    }
    if usage.tokens_output > 0 {
        parts.push(format!("{} out", usage.tokens_output));
    }
    if usage.tokens_reasoning > 0 {
        parts.push(format!("{} reasoning", usage.tokens_reasoning));
    }
    if parts.is_empty() {
        "turn complete".to_string()
    } else {
        parts.join(", ")
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

    #[test]
    fn separator_includes_usage_when_available() {
        let cell = TurnSeparatorCell::new(TurnUsage {
            tokens_input: 12,
            tokens_output: 3,
            ..TurnUsage::default()
        });
        assert_eq!(
            line_text(&cell.display_lines(30)[0]),
            "─ 12 in, 3 out ───────────────"
        );
    }
}
