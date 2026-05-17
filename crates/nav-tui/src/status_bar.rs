//! Bottom status bar.
//!
//! Renders a single row showing model, working directory, git branch, and
//! agent state (`Ready` or `Working …s`). Mirrors the codex CLI status row.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use std::path::Path;
use std::time::Duration;

pub struct StatusBar<'a> {
    pub model: &'a str,
    pub cwd_short: &'a str,
    pub branch: Option<&'a str>,
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

/// Read the current git branch by parsing `.git/HEAD` directly — fast,
/// dependency-free, and works the same whether `cwd` is the repo root or a
/// subdirectory (we walk up). Returns `None` when not in a repo.
pub fn git_branch(cwd: &Path) -> Option<String> {
    let mut dir = cwd;
    loop {
        let head = dir.join(".git").join("HEAD");
        if let Ok(contents) = std::fs::read_to_string(&head) {
            if let Some(rest) = contents.strip_prefix("ref: refs/heads/") {
                return Some(rest.trim().to_string());
            }
            return contents.trim().get(..7).map(str::to_string);
        }
        dir = dir.parent()?;
    }
}

/// Replace the user's home directory prefix with `~` so the status bar stays
/// readable in deep nested paths.
pub fn shorten_home(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}
