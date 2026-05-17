use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::history::HistoryCell;
use crate::streaming::StreamController;

/// Soft-wrap `text` to `width - 2` columns and prefix each line with a
/// two-space indent. A trailing newline is stripped so callers that
/// concatenate slices (e.g. stable + tail in a stream) don't see a phantom
/// blank line at the join. ASCII-only — grapheme-aware wrapping can replace
/// this one helper.
pub(crate) fn render_body(text: &str, width: u16) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let body_width = width.saturating_sub(2) as usize;
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let mut out = Vec::new();
    for raw_line in trimmed.split('\n') {
        if body_width == 0 {
            out.push(Line::from(format!("  {raw_line}")));
            continue;
        }
        let mut chunk_start = 0;
        let mut count = 0;
        let mut produced = false;
        for (idx, _) in raw_line.char_indices() {
            if count == body_width {
                out.push(Line::from(format!("  {}", &raw_line[chunk_start..idx])));
                chunk_start = idx;
                count = 0;
                produced = true;
            }
            count += 1;
        }
        if !produced || chunk_start < raw_line.len() {
            out.push(Line::from(format!("  {}", &raw_line[chunk_start..])));
        }
    }
    out
}

/// Replace the leading two-space pad on `lines[0]` with a styled gutter glyph
/// (e.g. `›` / `•` / `└`). Continuation lines keep the two-space pad so body
/// text aligns under the glyph at column 2.
fn apply_gutter_glyph(lines: &mut [Line<'static>], glyph: &str, style: Style) {
    let Some(first) = lines.first_mut() else {
        return;
    };
    let Some(span) = first.spans.first_mut() else {
        return;
    };
    let rest = span.content.strip_prefix("  ").unwrap_or(&span.content);
    let rest_owned = rest.to_string();
    let trailing = std::mem::take(&mut first.spans).into_iter().skip(1);
    let mut spans = Vec::with_capacity(2);
    spans.push(Span::styled(format!("{glyph} "), style));
    spans.push(Span::raw(rest_owned));
    spans.extend(trailing);
    first.spans = spans;
}

fn body_cell(glyph: &str, style: Style, body: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = render_body(body, width);
    if lines.is_empty() {
        lines.push(Line::from("  ".to_string()));
    }
    apply_gutter_glyph(&mut lines, glyph, style);
    lines.push(Line::from(String::new()));
    lines
}

/// Welcome card injected as the first transcript entry. Shows the model,
/// working directory, and session id, plus a short hint about slash commands
/// so the empty alt-screen doesn't read as a frozen blank.
pub struct WelcomeCell {
    model: String,
    cwd: String,
    session_id: String,
}

impl WelcomeCell {
    pub fn new(
        model: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            cwd: cwd.into(),
            session_id: session_id.into(),
        }
    }
}

impl HistoryCell for WelcomeCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let dim = Style::default().fg(Color::DarkGray);
        let value = Style::default().fg(Color::White);
        let accent = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let session_short: String = self.session_id.chars().take(10).collect();
        vec![
            Line::from(vec![
                Span::styled("  nav", accent),
                Span::styled("  ·  ", dim),
                Span::styled(self.model.clone(), value),
                Span::styled("  ·  ", dim),
                Span::styled(self.cwd.clone(), value),
                Span::styled("  ·  session ", dim),
                Span::styled(session_short, value),
            ]),
            Line::from(String::new()),
            Line::from(Span::styled(
                "  Type a prompt to begin. Slash commands:".to_string(),
                dim,
            )),
            Line::from(vec![
                Span::styled("    /quit", dim),
                Span::styled("      exit".to_string(), dim),
            ]),
            Line::from(vec![
                Span::styled("    /clear", dim),
                Span::styled("     start a new transcript".to_string(), dim),
            ]),
            Line::from(vec![
                Span::styled("    /sessions", dim),
                Span::styled("  not wired yet".to_string(), dim),
            ]),
            Line::from(String::new()),
        ]
    }
}

pub struct UserMessageCell {
    text: String,
}

impl UserMessageCell {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl HistoryCell for UserMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        body_cell(
            "›",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            &self.text,
            width,
        )
    }
}

pub struct AssistantMessageCell {
    controller: StreamController,
}

impl AssistantMessageCell {
    pub fn new(text: impl Into<String>) -> Self {
        let mut controller = StreamController::default();
        controller.push_delta(&text.into());
        controller.finalize();
        Self { controller }
    }

    pub fn streaming() -> Self {
        Self {
            controller: StreamController::default(),
        }
    }

    pub fn push_delta(&mut self, text: &str) {
        self.controller.push_delta(text);
    }

    pub fn finalize(&mut self) {
        self.controller.finalize();
    }
}

impl HistoryCell for AssistantMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let (stable, tail) = self.controller.partitioned_lines(width);
        let mut out: Vec<Line<'static>> = Vec::with_capacity(stable.len() + tail.len() + 1);
        out.extend(stable);
        out.extend(tail);
        if out.is_empty() {
            out.push(Line::from("  ".to_string()));
        }
        apply_gutter_glyph(
            &mut out,
            "•",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        );
        out.push(Line::from(String::new()));
        out
    }
}

pub struct ToolCallCell {
    summary: String,
}

impl ToolCallCell {
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        let args =
            serde_json::to_string(&arguments).unwrap_or_else(|_| "<unserializable>".to_string());
        Self {
            summary: format!("{} {}", name.into(), args),
        }
    }
}

impl HistoryCell for ToolCallCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        body_cell(
            "•",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            &self.summary,
            width,
        )
    }
}

pub struct ToolOutputCell {
    output: String,
    is_error: bool,
}

impl ToolOutputCell {
    pub fn new(output: impl Into<String>, is_error: bool) -> Self {
        Self {
            output: output.into(),
            is_error,
        }
    }
}

impl HistoryCell for ToolOutputCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let (glyph, style) = if self.is_error {
            (
                "└",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )
        } else {
            ("└", Style::default().fg(Color::DarkGray))
        };
        body_cell(glyph, style, &self.output, width)
    }
}

pub struct ErrorCell {
    message: String,
}

impl ErrorCell {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl HistoryCell for ErrorCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        body_cell(
            "•",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            &self.message,
            width,
        )
    }
}
