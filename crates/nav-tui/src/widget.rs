use nav_core::AgentEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

use crate::cells::{
    AssistantMessageCell, ErrorCell, ToolCallCell, ToolOutputCell, UserMessageCell,
};
use crate::history::HistoryCell;

/// Flat scrollback widget. Holds the full history as a vector of cells and
/// stacks them top-to-bottom inside its render area. Anything that does not
/// fit is silently clipped at the bottom — scrolling and viewport tracking
/// belong to a later slice.
pub struct ChatWidget {
    cells: Vec<Box<dyn HistoryCell>>,
}

impl ChatWidget {
    pub fn new() -> Self {
        Self { cells: Vec::new() }
    }

    /// Append a user-authored message. User input is not delivered through
    /// `AgentEvent`, so it has its own entry point.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.cells.push(Box::new(UserMessageCell::new(text)));
    }

    /// Translate an agent event into a history cell and append it.
    ///
    /// `AssistantMessageDelta` and `TurnComplete` are intentionally ignored:
    /// streaming-partition and a status surface arrive in later slices.
    pub fn ingest(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::AssistantMessageDelta { .. } | AgentEvent::TurnComplete { .. } => {}
            AgentEvent::AssistantMessageDone { text } => {
                self.cells.push(Box::new(AssistantMessageCell::new(text)));
            }
            AgentEvent::ToolCallStarted {
                call_id: _,
                name,
                arguments,
            } => {
                self.cells
                    .push(Box::new(ToolCallCell::new(name, arguments)));
            }
            AgentEvent::ToolCallOutput {
                call_id: _,
                output,
                is_error,
            } => {
                self.cells
                    .push(Box::new(ToolOutputCell::new(output, is_error)));
            }
            AgentEvent::Error { message } => {
                self.cells.push(Box::new(ErrorCell::new(message)));
            }
        }
    }
}

impl Default for ChatWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &ChatWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y;
        for cell in &self.cells {
            if y >= area.bottom() {
                break;
            }
            let lines = cell.display_lines(area.width);
            if lines.is_empty() {
                continue;
            }
            let remaining = area.bottom() - y;
            let h = (lines.len() as u16).min(remaining);
            let rect = Rect {
                x: area.x,
                y,
                width: area.width,
                height: h,
            };
            Paragraph::new(lines).render(rect, buf);
            y = y.saturating_add(h);
        }
    }
}
