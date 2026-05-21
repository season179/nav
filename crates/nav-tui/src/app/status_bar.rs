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
    /// Current context-window occupancy: the latest provider-reported
    /// `tokens_input`. `0` before the first `TurnComplete`.
    pub tokens_input: u64,
    /// Effective context window used to compute the percentage. `0` hides the
    /// gauge entirely.
    pub context_window: u64,
}

/// Format `tokens` as `<n.n>k` (one decimal). Caller must gate on
/// `>= 1_000`; below that the gauge is hidden entirely.
fn format_tokens_k(tokens: u64) -> String {
    debug_assert!(tokens >= 1_000);
    let tenths = (tokens + 50) / 100;
    let whole = tenths / 10;
    let frac = tenths % 10;
    if frac == 0 {
        format!("{whole}k")
    } else {
        format!("{whole}.{frac}k")
    }
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
        if let Some(pct) = self
            .tokens_input
            .saturating_mul(100)
            .checked_div(self.context_window)
            && self.tokens_input >= 1_000
        {
            let denom_k = (self.context_window + 500) / 1_000;
            spans.push(Span::styled("  ·  ", dim));
            spans.push(Span::styled(
                format!(
                    "{}/{denom_k}k {pct}%",
                    format_tokens_k(self.tokens_input)
                ),
                dim,
            ));
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}
