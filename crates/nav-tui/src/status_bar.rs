//! Bottom status bar.
//!
//! Renders a single row showing model, working directory, git branch, and
//! agent state (`Ready` or `Working …s`). Mirrors the codex CLI status row.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use std::time::Duration;

pub struct StatusBar<'a> {
    pub model: &'a str,
    pub cwd_short: &'a str,
    pub branch: Option<&'a str>,
    /// Append a yellow `✱` after the branch span when the worktree has
    /// uncommitted changes. False when not in a repo.
    pub dirty: bool,
    pub state: AgentState,
}

pub enum AgentState {
    Ready,
    Working { elapsed: Duration, spinner: char },
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let dim = Style::default().fg(Color::DarkGray);
        let sep = Span::styled("  ·  ", dim);
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled("  ", dim),
            Span::styled(self.model.to_string(), dim),
            sep.clone(),
            Span::styled(
                self.cwd_short.to_string(),
                Style::default().fg(Color::Yellow),
            ),
        ];
        if let Some(branch) = self.branch {
            spans.push(sep.clone());
            spans.push(Span::styled(
                branch.to_string(),
                Style::default().fg(Color::Green),
            ));
            if self.dirty {
                spans.push(Span::styled(
                    " ✱".to_string(),
                    Style::default().fg(Color::Yellow),
                ));
            }
        }
        spans.push(sep);
        match self.state {
            AgentState::Ready => {
                spans.push(Span::styled(
                    "Ready",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            AgentState::Working { elapsed, spinner } => {
                let secs = elapsed.as_secs();
                spans.push(Span::styled(
                    format!("{spinner} Working {secs}s"),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ));
            }
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}
