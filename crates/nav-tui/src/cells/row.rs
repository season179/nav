use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::DEFAULT_COMPOSER_BG;

use super::wrapping::render_body;

const BODY_INDENT: &str = "  ";
const BODY_INDENT_WIDTH: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptRowKind {
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolOutput,
    ToolError,
    SkillInvocation,
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
    Notice,
    TurnWarning,
    ApprovalApproved,
    ApprovalDenied,
    ApprovalAborted,
    CompactionStarted,
    CompactionCompleted,
    CompactionFailed,
    HookCompact,
    HookOutput,
    HookFailed,
    Error,
    Reasoning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TranscriptRowStyle {
    glyph: &'static str,
    label: &'static str,
    color: Color,
    layout: TranscriptRowLayout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscriptRowLayout {
    Bullet,
    Labeled,
    UserBox { surface: Color },
}

impl TranscriptRowKind {
    pub(crate) fn style(self) -> TranscriptRowStyle {
        match self {
            Self::UserMessage => TranscriptRowStyle::user_box("›", DEFAULT_COMPOSER_BG),
            Self::AssistantMessage => TranscriptRowStyle::bullet("•", Color::White),
            Self::ToolCall => TranscriptRowStyle::labeled("•", "Running", Color::Yellow),
            Self::ToolOutput => TranscriptRowStyle::labeled("•", "Ran", Color::Green),
            Self::ToolError => TranscriptRowStyle::labeled("■", "Failed", Color::Red),
            Self::SkillInvocation => TranscriptRowStyle::labeled("◆", "skill", Color::Magenta),
            Self::SessionNotice => TranscriptRowStyle::labeled("◆", "notice", Color::Cyan),
            Self::SessionTree => TranscriptRowStyle::labeled("◆", "tree", Color::Cyan),
            Self::TranscriptHits => TranscriptRowStyle::labeled("◆", "find", Color::Cyan),
            Self::SubagentStarted => TranscriptRowStyle::labeled("*", "subagent", Color::Blue),
            Self::SubagentCompleted => TranscriptRowStyle::labeled("*", "subagent", Color::Green),
            Self::SubagentFailed => TranscriptRowStyle::labeled("*", "subagent", Color::Red),
            Self::FileChanged => TranscriptRowStyle::labeled("◆", "changed", Color::Cyan),
            Self::FileChangeFailed => TranscriptRowStyle::labeled("◆", "failed", Color::Red),
            Self::TurnDiff => TranscriptRowStyle::labeled("◆", "diff", Color::Blue),
            Self::GitCheckpoint => TranscriptRowStyle::labeled("◆", "checkpoint", Color::Cyan),
            Self::GitStash => TranscriptRowStyle::labeled("◆", "stash", Color::Magenta),
            Self::GitRestore => TranscriptRowStyle::labeled("◆", "restore", Color::Green),
            Self::PendingQueued => TranscriptRowStyle::labeled("◆", "queued", Color::Blue),
            Self::PendingEdited => TranscriptRowStyle::labeled("◆", "edited", Color::Blue),
            Self::PendingRemoved => TranscriptRowStyle::labeled("◆", "removed", Color::DarkGray),
            Self::PendingCleared => TranscriptRowStyle::labeled("◆", "cleared", Color::DarkGray),
            Self::PendingDequeued => TranscriptRowStyle::labeled("◆", "dequeued", Color::Blue),
            Self::TurnAborted => TranscriptRowStyle::labeled("◆", "aborted", Color::Red),
            Self::Notice => TranscriptRowStyle::bullet("•", Color::Cyan),
            Self::TurnWarning => TranscriptRowStyle::bullet("!", Color::Yellow),
            Self::ApprovalApproved => TranscriptRowStyle::bullet("✓", Color::Green),
            Self::ApprovalDenied => TranscriptRowStyle::bullet("!", Color::Yellow),
            Self::ApprovalAborted => TranscriptRowStyle::bullet("■", Color::Red),
            Self::CompactionStarted => TranscriptRowStyle::labeled("◆", "compact", Color::Magenta),
            Self::CompactionCompleted => {
                TranscriptRowStyle::labeled("◆", "compacted", Color::Magenta)
            }
            Self::CompactionFailed => TranscriptRowStyle::labeled("◆", "compact!", Color::Red),
            Self::HookCompact => TranscriptRowStyle::bullet("✓", Color::DarkGray),
            Self::HookOutput => TranscriptRowStyle::labeled("◆", "hook", Color::Cyan),
            Self::HookFailed => TranscriptRowStyle::labeled("■", "hook", Color::Red),
            Self::Error => TranscriptRowStyle::bullet("■", Color::Red),
            Self::Reasoning => {
                TranscriptRowStyle::labeled("◆", "reasoning", Color::DarkGray)
            }
        }
    }
}

impl TranscriptRowStyle {
    const fn bullet(glyph: &'static str, color: Color) -> Self {
        Self {
            glyph,
            label: "",
            color,
            layout: TranscriptRowLayout::Bullet,
        }
    }

    const fn labeled(glyph: &'static str, label: &'static str, color: Color) -> Self {
        Self {
            glyph,
            label,
            color,
            layout: TranscriptRowLayout::Labeled,
        }
    }

    const fn user_box(glyph: &'static str, surface: Color) -> Self {
        Self {
            glyph,
            label: "",
            color: Color::White,
            layout: TranscriptRowLayout::UserBox { surface },
        }
    }

    fn with_user_surface(self, surface: Color) -> Self {
        match self.layout {
            TranscriptRowLayout::UserBox { .. } => Self {
                layout: TranscriptRowLayout::UserBox { surface },
                ..self
            },
            TranscriptRowLayout::Bullet | TranscriptRowLayout::Labeled => self,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        self.label
    }

    pub(crate) fn body_width(self, width: u16, label: &str) -> u16 {
        body_width_for_prefix(width, self.prefix_width(label))
    }

    fn prefix_width(self, label: &str) -> usize {
        match self.layout {
            TranscriptRowLayout::Bullet => self.glyph.chars().count().saturating_add(1),
            TranscriptRowLayout::Labeled => labeled_prefix_width(self.glyph, label),
            TranscriptRowLayout::UserBox { .. } => self.glyph.chars().count().saturating_add(1),
        }
    }
}

pub(crate) struct TranscriptRow<'a> {
    style: TranscriptRowStyle,
    label: Cow<'a, str>,
    body: Cow<'a, str>,
}

impl<'a> TranscriptRow<'a> {
    pub(crate) fn new(kind: TranscriptRowKind, body: impl Into<Cow<'a, str>>) -> Self {
        let style = kind.style();
        Self {
            style,
            label: Cow::Borrowed(style.label()),
            body: body.into(),
        }
    }

    pub(crate) fn with_label(
        kind: TranscriptRowKind,
        label: impl Into<Cow<'a, str>>,
        body: impl Into<Cow<'a, str>>,
    ) -> Self {
        let style = kind.style();
        Self {
            style,
            label: label.into(),
            body: body.into(),
        }
    }

    pub(crate) fn user_message(body: impl Into<Cow<'a, str>>, surface: Color) -> Self {
        let style = TranscriptRowKind::UserMessage
            .style()
            .with_user_surface(surface);
        Self {
            style,
            label: Cow::Borrowed(""),
            body: body.into(),
        }
    }

    fn body_width(&self, width: u16) -> u16 {
        self.style.body_width(width, &self.label)
    }

    pub(crate) fn render(self, width: u16) -> Vec<Line<'static>> {
        let lines = render_body(&self.body, self.body_width(width));
        match self.style.layout {
            TranscriptRowLayout::Bullet => finish_bullet_row_lines(self.style, lines),
            TranscriptRowLayout::Labeled => {
                finish_labeled_row_lines(self.style, &self.label, lines)
            }
            TranscriptRowLayout::UserBox { surface } => {
                finish_user_box_lines(self.style, lines, width, surface)
            }
        }
    }
}

/// Replace the leading two-space body indent on `lines[0]` with styled row
/// chrome. Continuation lines keep the indent so body text has a stable gutter.
fn apply_gutter_header(lines: &mut [Line<'static>], style: TranscriptRowStyle, label: &str) {
    let Some(first) = lines.first_mut() else {
        return;
    };
    if first.spans.is_empty() {
        return;
    }

    // The first span is expected to be the "  " body indent. After stripping
    // it, any remaining text is body content; subsequent spans (from inline
    // markdown parsing) are preserved as-is.
    let first_content = first.spans[0].content.clone();
    let rest_owned = if let Some(rest) = first_content.strip_prefix(BODY_INDENT) {
        rest.to_string()
    } else {
        first_content.to_string()
    };

    let mut trailing: Vec<_> = std::mem::take(&mut first.spans).into_iter().skip(1).collect();
    let has_body = !rest_owned.is_empty() || !trailing.is_empty();
    let mut spans = row_header_spans(style, label, has_body);
    spans.push(Span::raw(rest_owned));
    spans.append(&mut trailing);
    first.spans = spans;
}

fn row_header_spans(style: TranscriptRowStyle, label: &str, has_body: bool) -> Vec<Span<'static>> {
    match style.layout {
        TranscriptRowLayout::Bullet => bullet_header_spans(style),
        TranscriptRowLayout::Labeled => labeled_header_spans(style, label, has_body),
        TranscriptRowLayout::UserBox { .. } => user_box_header_spans(style),
    }
}

fn bullet_header_spans(style: TranscriptRowStyle) -> Vec<Span<'static>> {
    vec![Span::styled(
        format!("{} ", style.glyph),
        label_style(style.color),
    )]
}

fn labeled_header_spans(
    style: TranscriptRowStyle,
    label: &str,
    has_body: bool,
) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(4);
    let label_style = label_style(style.color);
    spans.push(Span::styled(format!("{} ", style.glyph), label_style));
    spans.push(Span::styled(label.to_string(), label_style));
    if has_body {
        spans.push(Span::styled(
            "  ".to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

fn user_box_header_spans(style: TranscriptRowStyle) -> Vec<Span<'static>> {
    let prompt_style = Style::default()
        .fg(style.color)
        .add_modifier(Modifier::BOLD);
    vec![Span::styled(format!("{} ", style.glyph), prompt_style)]
}

fn finish_user_box_lines(
    style: TranscriptRowStyle,
    mut lines: Vec<Line<'static>>,
    width: u16,
    surface: Color,
) -> Vec<Line<'static>> {
    if lines.is_empty() {
        lines.push(Line::from(BODY_INDENT.to_string()));
    }
    apply_gutter_header(&mut lines, style, "");

    let mut out = Vec::with_capacity(lines.len() + 3);
    out.push(user_box_padding_line(width, surface));
    out.extend(lines.into_iter().map(|mut line| {
        style_user_box_line(&mut line, width, surface);
        line
    }));
    out.push(user_box_padding_line(width, surface));
    out.push(Line::from(String::new()));
    out
}

fn user_box_padding_line(width: u16, surface: Color) -> Line<'static> {
    let mut line = Line::from(Span::raw(" ".repeat(width as usize)));
    line.style = user_box_style(surface);
    line
}

fn style_user_box_line(line: &mut Line<'static>, width: u16, surface: Color) {
    line.style = user_box_style(surface);
    let used_width = line_text_width(line);
    let target_width = width as usize;
    if target_width > used_width {
        line.spans
            .push(Span::raw(" ".repeat(target_width - used_width)));
    }
}

fn user_box_style(surface: Color) -> Style {
    Style::default().fg(Color::White).bg(surface)
}

fn line_text_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum()
}

pub(crate) fn finish_row_lines(
    kind: TranscriptRowKind,
    label: &str,
    lines: Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    let style = kind.style();
    match style.layout {
        TranscriptRowLayout::Bullet => finish_bullet_row_lines(style, lines),
        TranscriptRowLayout::Labeled => finish_labeled_row_lines(style, label, lines),
        TranscriptRowLayout::UserBox { .. } => {
            unreachable!(
                "user message rows need the caller's width; use TranscriptRow::user_message"
            )
        }
    }
}

fn finish_bullet_row_lines(
    style: TranscriptRowStyle,
    mut lines: Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    if lines.is_empty() {
        lines.push(Line::from(BODY_INDENT.to_string()));
    }
    apply_gutter_header(&mut lines, style, "");
    lines.push(Line::from(String::new()));
    lines
}

fn finish_labeled_row_lines(
    style: TranscriptRowStyle,
    label: &str,
    mut lines: Vec<Line<'static>>,
) -> Vec<Line<'static>> {
    if lines.is_empty() {
        lines.push(Line::from(BODY_INDENT.to_string()));
    }
    apply_gutter_header(&mut lines, style, label);
    lines.push(Line::from(String::new()));
    lines
}

fn label_style(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn body_width_for_prefix(width: u16, prefix_width: usize) -> u16 {
    let extra_width = prefix_width.saturating_sub(BODY_INDENT_WIDTH);
    let extra_width = u16::try_from(extra_width).unwrap_or(u16::MAX);
    width.saturating_sub(extra_width)
}

fn labeled_prefix_width(glyph: &str, label: &str) -> usize {
    glyph
        .chars()
        .count()
        .saturating_add(1)
        .saturating_add(label.chars().count())
        .saturating_add(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn first_line_text(kind: TranscriptRowKind, body: &str) -> String {
        let lines = TranscriptRow::new(kind, body).render(80);
        line_text(&lines[0])
    }

    #[test]
    fn core_semantic_rows_have_distinct_stable_chrome() {
        assert_eq!(
            first_line_text(TranscriptRowKind::AssistantMessage, "hello"),
            "• hello"
        );
        assert_eq!(
            first_line_text(TranscriptRowKind::ToolCall, "bash ls"),
            "• Running  bash ls"
        );
        assert_eq!(
            first_line_text(TranscriptRowKind::ToolOutput, "2 lines"),
            "• Ran  2 lines"
        );
        assert_eq!(
            first_line_text(TranscriptRowKind::ToolError, "permission denied"),
            "■ Failed  permission denied"
        );
        assert_eq!(
            first_line_text(TranscriptRowKind::Error, "permission denied"),
            "■ permission denied"
        );
    }

    #[test]
    fn user_message_uses_composer_surface_box() {
        let surface = Color::Rgb(1, 2, 3);
        let lines = TranscriptRow::user_message("hello", surface).render(12);

        assert_eq!(line_text(&lines[0]).chars().count(), 12);
        assert_eq!(line_text(&lines[1]).trim_end(), "› hello");
        assert_eq!(line_text(&lines[1]).chars().count(), 12);
        assert_eq!(lines[0].style.bg, Some(surface));
        assert_eq!(lines[1].style.bg, Some(surface));
        assert_eq!(lines[2].style.bg, Some(surface));
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
