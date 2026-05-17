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

struct LabeledBlock {
    label: &'static str,
    color: Color,
    body: String,
}

impl LabeledBlock {
    fn new(label: &'static str, color: Color, body: impl Into<String>) -> Self {
        Self {
            label,
            color,
            body: body.into(),
        }
    }

    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut out = Vec::with_capacity(2);
        out.push(Line::from(Span::styled(
            self.label.to_string(),
            Style::default().fg(self.color).add_modifier(Modifier::BOLD),
        )));
        out.extend(render_body(&self.body, width));
        out.push(Line::from(String::new()));
        out
    }
}

pub struct UserMessageCell(LabeledBlock);

impl UserMessageCell {
    pub fn new(text: impl Into<String>) -> Self {
        Self(LabeledBlock::new("user", Color::Cyan, text))
    }
}

impl HistoryCell for UserMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.0.lines(width)
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
        let mut out = Vec::with_capacity(2 + stable.len() + tail.len());
        out.push(Line::from(Span::styled(
            "assistant".to_string(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
        out.extend(stable);
        out.extend(tail);
        out.push(Line::from(String::new()));
        out
    }
}

pub struct ToolCallCell(LabeledBlock);

impl ToolCallCell {
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        let args =
            serde_json::to_string(&arguments).unwrap_or_else(|_| "<unserializable>".to_string());
        let summary = format!("tool · {} {}", name.into(), args);
        Self(LabeledBlock::new("tool call", Color::Yellow, summary))
    }
}

impl HistoryCell for ToolCallCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.0.lines(width)
    }
}

pub struct ToolOutputCell(LabeledBlock);

impl ToolOutputCell {
    pub fn new(output: impl Into<String>, is_error: bool) -> Self {
        let (label, color) = if is_error {
            ("tool error", Color::Red)
        } else {
            ("tool output", Color::DarkGray)
        };
        Self(LabeledBlock::new(label, color, output))
    }
}

impl HistoryCell for ToolOutputCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.0.lines(width)
    }
}

pub struct ErrorCell(LabeledBlock);

impl ErrorCell {
    pub fn new(message: impl Into<String>) -> Self {
        Self(LabeledBlock::new("error", Color::Red, message))
    }
}

impl HistoryCell for ErrorCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.0.lines(width)
    }
}
