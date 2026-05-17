use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::history::HistoryCell;

/// Shared layout for every cell: a colored header label, an indented body
/// block, and a trailing blank line. The body is wrapped to fit `width` on
/// character boundaries — fine for ASCII transcripts; a future slice can
/// swap in grapheme-aware wrapping inside this one place.
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
        let body_width = width.saturating_sub(2) as usize;
        let mut out = Vec::new();
        out.push(Line::from(Span::styled(
            self.label.to_string(),
            Style::default().fg(self.color).add_modifier(Modifier::BOLD),
        )));
        for raw_line in self.body.split('\n') {
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

pub struct AssistantMessageCell(LabeledBlock);

impl AssistantMessageCell {
    pub fn new(text: impl Into<String>) -> Self {
        Self(LabeledBlock::new("assistant", Color::Green, text))
    }
}

impl HistoryCell for AssistantMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.0.lines(width)
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
