use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::wrapping::render_body;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptRowKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolOutput,
    ToolError,
    SkillInvocation,
    SessionList,
    SessionNotice,
    SessionTree,
    TranscriptHits,
    SubagentStarted,
    SubagentCompleted,
    SubagentFailed,
    FileChanged,
    FileChangeFailed,
    TurnDiff,
    GitCheckpoint,
    GitStash,
    GitRestore,
    PendingQueued,
    PendingEdited,
    PendingRemoved,
    PendingCleared,
    PendingDequeued,
    TurnAborted,
    CompactionStarted,
    CompactionCompleted,
    CompactionFailed,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranscriptRowStyle {
    glyph: &'static str,
    label: &'static str,
    color: Color,
}

impl TranscriptRowKind {
    pub(crate) fn style(self) -> TranscriptRowStyle {
        match self {
            Self::UserMessage => TranscriptRowStyle::new("›", "user", Color::Cyan),
            Self::AssistantMessage => TranscriptRowStyle::new("•", "assistant", Color::Green),
            Self::ToolCall => TranscriptRowStyle::new("•", "tool", Color::Yellow),
            Self::ToolOutput => TranscriptRowStyle::new("└", "output", Color::DarkGray),
            Self::ToolError => TranscriptRowStyle::new("└", "error", Color::Red),
            Self::SkillInvocation => TranscriptRowStyle::new("◆", "skill", Color::Magenta),
            Self::SessionList => TranscriptRowStyle::new("◆", "sessions", Color::Cyan),
            Self::SessionNotice => TranscriptRowStyle::new("◆", "notice", Color::Cyan),
            Self::SessionTree => TranscriptRowStyle::new("◆", "tree", Color::Cyan),
            Self::TranscriptHits => TranscriptRowStyle::new("◆", "find", Color::Cyan),
            Self::SubagentStarted => TranscriptRowStyle::new("*", "subagent", Color::Blue),
            Self::SubagentCompleted => TranscriptRowStyle::new("*", "subagent", Color::Green),
            Self::SubagentFailed => TranscriptRowStyle::new("*", "subagent", Color::Red),
            Self::FileChanged => TranscriptRowStyle::new("◆", "changed", Color::Cyan),
            Self::FileChangeFailed => TranscriptRowStyle::new("◆", "failed", Color::Red),
            Self::TurnDiff => TranscriptRowStyle::new("◆", "diff", Color::Blue),
            Self::GitCheckpoint => TranscriptRowStyle::new("◆", "checkpoint", Color::Cyan),
            Self::GitStash => TranscriptRowStyle::new("◆", "stash", Color::Magenta),
            Self::GitRestore => TranscriptRowStyle::new("◆", "restore", Color::Green),
            Self::PendingQueued => TranscriptRowStyle::new("◆", "queued", Color::Blue),
            Self::PendingEdited => TranscriptRowStyle::new("◆", "edited", Color::Blue),
            Self::PendingRemoved => TranscriptRowStyle::new("◆", "removed", Color::DarkGray),
            Self::PendingCleared => TranscriptRowStyle::new("◆", "cleared", Color::DarkGray),
            Self::PendingDequeued => TranscriptRowStyle::new("◆", "dequeued", Color::Blue),
            Self::TurnAborted => TranscriptRowStyle::new("◆", "aborted", Color::Red),
            Self::CompactionStarted => TranscriptRowStyle::new("◆", "compact", Color::Magenta),
            Self::CompactionCompleted => TranscriptRowStyle::new("◆", "compacted", Color::Magenta),
            Self::CompactionFailed => TranscriptRowStyle::new("◆", "compact!", Color::Red),
            Self::Error => TranscriptRowStyle::new("•", "error", Color::Red),
        }
    }
}

impl TranscriptRowStyle {
    const fn new(glyph: &'static str, label: &'static str, color: Color) -> Self {
        Self {
            glyph,
            label,
            color,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        self.label
    }
}

pub(crate) struct TranscriptRow<'a> {
    kind: TranscriptRowKind,
    label: Cow<'a, str>,
    body: Cow<'a, str>,
}

impl<'a> TranscriptRow<'a> {
    pub(crate) fn new(kind: TranscriptRowKind, body: impl Into<Cow<'a, str>>) -> Self {
        let style = kind.style();
        Self {
            kind,
            label: Cow::Borrowed(style.label()),
            body: body.into(),
        }
    }

    pub(crate) fn with_label(
        kind: TranscriptRowKind,
        label: impl Into<Cow<'a, str>>,
        body: impl Into<Cow<'a, str>>,
    ) -> Self {
        Self {
            kind,
            label: label.into(),
            body: body.into(),
        }
    }

    fn body_width(&self, width: u16) -> u16 {
        body_width_for_label(width, &self.label)
    }

    pub(crate) fn render(self, width: u16) -> Vec<Line<'static>> {
        let lines = render_body(&self.body, self.body_width(width));
        finish_row_lines(self.kind, &self.label, lines)
    }
}

/// Replace the leading two-space pad on `lines[0]` with a styled row header.
/// Continuation lines keep the two-space pad so body text aligns under the
/// transcript gutter for every semantic row type.
fn apply_gutter_header(lines: &mut [Line<'static>], style: TranscriptRowStyle, label: &str) {
    let Some(first) = lines.first_mut() else {
        return;
    };
    let Some(span) = first.spans.first_mut() else {
        return;
    };
    let rest = span.content.strip_prefix("  ").unwrap_or(&span.content);
    let rest_owned = rest.to_string();
    let trailing = std::mem::take(&mut first.spans).into_iter().skip(1);
    let mut spans = Vec::with_capacity(4);
    let label_style = label_style(style.color);
    spans.push(Span::styled(format!("{} ", style.glyph), label_style));
    spans.push(Span::styled(label.to_string(), label_style));
    if !rest_owned.is_empty() {
        spans.push(Span::styled(
            "  ".to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans.push(Span::raw(rest_owned));
    spans.extend(trailing);
    first.spans = spans;
}

pub(crate) fn finish_row_lines(
    kind: TranscriptRowKind,
    label: &str,
    mut lines: Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    if lines.is_empty() {
        lines.push(Line::from("  ".to_string()));
    }
    apply_gutter_header(&mut lines, kind.style(), label);
    lines.push(Line::from(String::new()));
    lines
}

fn label_style(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub(crate) fn body_width_for_label(width: u16, label: &str) -> u16 {
    let header_width = 1usize
        .saturating_add(1)
        .saturating_add(label.chars().count())
        .saturating_add(2);
    let extra_width = header_width.saturating_sub(2);
    let extra_width = u16::try_from(extra_width).unwrap_or(u16::MAX);
    width.saturating_sub(extra_width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_line_text(kind: TranscriptRowKind, body: &str) -> String {
        let lines = TranscriptRow::new(kind, body).render(80);
        lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn core_semantic_rows_have_distinct_stable_chrome() {
        assert_eq!(
            first_line_text(TranscriptRowKind::UserMessage, "hello"),
            "› user  hello"
        );
        assert_eq!(
            first_line_text(TranscriptRowKind::AssistantMessage, "hello"),
            "• assistant  hello"
        );
        assert_eq!(
            first_line_text(TranscriptRowKind::ToolCall, "bash ls"),
            "• tool  bash ls"
        );
        assert_eq!(
            first_line_text(TranscriptRowKind::ToolOutput, "2 lines"),
            "└ output  2 lines"
        );
        assert_eq!(
            first_line_text(TranscriptRowKind::ToolError, "permission denied"),
            "└ error  permission denied"
        );
    }

    #[test]
    fn session_notice_can_keep_command_specific_label() {
        let lines = TranscriptRow::with_label(TranscriptRowKind::SessionNotice, "export", "saved")
            .render(80);
        let first = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(first, "◆ export  saved");
    }
}
