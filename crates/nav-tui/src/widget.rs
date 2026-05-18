use nav_core::AgentEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

use crate::cells::{
    AssistantMessageCell, ErrorCell, ToolCallCell, ToolOutputCell, UserMessageCell, WelcomeCell,
};
use crate::history::HistoryCell;

/// Flat scrollback widget. Holds the full history as a vector of cells and
/// renders a viewport over their flattened lines.
pub struct ChatWidget {
    cells: Vec<Box<dyn HistoryCell>>,
    /// `None` follows the newest transcript row. `Some(row)` pins the top of
    /// the viewport while the user is reading older output.
    scroll_top: Option<usize>,
}

impl ChatWidget {
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            scroll_top: None,
        }
    }

    /// Append a user-authored message. User input is not delivered through
    /// `AgentEvent`, so it has its own entry point.
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.cells.push(Box::new(UserMessageCell::new(text)));
    }

    /// Prepend a welcome cell that orients the user (model, cwd, session id).
    /// Called at TUI launch on a fresh session.
    pub fn push_welcome(
        &mut self,
        model: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
    ) {
        self.cells
            .push(Box::new(WelcomeCell::new(model, cwd, session_id)));
    }

    /// Translate an agent event into a history cell and append it.
    ///
    /// `AssistantMessageDelta` is currently ignored: the TUI shows assistant
    /// text when `AssistantMessageDone` arrives. `TurnComplete` is handled by
    /// the status bar in `run()`, not by the scrollback widget.
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

    pub fn scroll_up(&mut self, rows: u16, width: u16, viewport_height: u16) {
        let max_top = self.max_top(width, viewport_height);
        if max_top == 0 {
            self.scroll_top = None;
            return;
        }
        let current = self.current_top(width, viewport_height);
        self.scroll_top = Some(current.saturating_sub(rows as usize));
    }

    pub fn scroll_down(&mut self, rows: u16, width: u16, viewport_height: u16) {
        let max_top = self.max_top(width, viewport_height);
        let current = self.current_top(width, viewport_height);
        let next = current.saturating_add(rows as usize);
        if next >= max_top {
            self.scroll_top = None;
        } else {
            self.scroll_top = Some(next);
        }
    }

    pub fn scroll_to_top(&mut self, width: u16, viewport_height: u16) {
        if self.max_top(width, viewport_height) == 0 {
            self.scroll_top = None;
        } else {
            self.scroll_top = Some(0);
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_top = None;
    }

    fn max_top(&self, width: u16, viewport_height: u16) -> usize {
        self.rendered_height(width)
            .saturating_sub(viewport_height as usize)
    }

    fn current_top(&self, width: u16, viewport_height: u16) -> usize {
        let max_top = self.max_top(width, viewport_height);
        self.scroll_top.unwrap_or(max_top).min(max_top)
    }

    fn rendered_height(&self, width: u16) -> usize {
        self.cells
            .iter()
            .map(|cell| cell.display_lines(width).len())
            .sum()
    }

    fn render_lines(&self, width: u16) -> Vec<ratatui::text::Line<'static>> {
        let mut lines = Vec::new();
        for cell in &self.cells {
            lines.extend(cell.display_lines(width));
        }
        lines
    }
}

impl Default for ChatWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &ChatWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let lines = self.render_lines(area.width);
        let total = lines.len();
        let viewport_height = area.height as usize;
        let max_scroll = total.saturating_sub(viewport_height);
        let start = self.scroll_top.unwrap_or(max_scroll).min(max_scroll);
        let visible: Vec<_> = lines
            .into_iter()
            .skip(start)
            .take(viewport_height)
            .collect();
        Paragraph::new(visible).render(area, buf);
    }
}
