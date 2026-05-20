use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::history::HistoryCell;

/// Welcome card injected as the first transcript entry. Shows the model,
/// working directory, and session id, plus a short hint about slash commands
/// so the empty alt-screen doesn't read as a frozen blank.
pub struct WelcomeCell {
    model: String,
    cwd: String,
    session_id: String,
    /// Optional `branch ✱ (dirty)` summary. Empty when not in a git repo.
    branch_summary: Option<String>,
    /// Optional `AGENTS.md (project), CLAUDE.md (user)` summary. Empty when
    /// no context files were discovered.
    context_summary: Option<String>,
    /// Optional `.nav/settings.json (project)` summary. Empty when no
    /// settings files were loaded.
    settings_summary: Option<String>,
}

impl WelcomeCell {
    pub fn new(
        model: impl Into<String>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
        branch_summary: Option<String>,
        context_summary: Option<String>,
        settings_summary: Option<String>,
    ) -> Self {
        Self {
            model: model.into(),
            cwd: cwd.into(),
            session_id: session_id.into(),
            branch_summary,
            context_summary,
            settings_summary,
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
        let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
            Span::styled("  nav", accent),
            Span::styled("  ·  ", dim),
            Span::styled(self.model.clone(), value),
            Span::styled("  ·  ", dim),
            Span::styled(self.cwd.clone(), value),
            Span::styled("  ·  session ", dim),
            Span::styled(session_short, value),
        ])];
        if let Some(branch) = &self.branch_summary {
            lines.push(Line::from(vec![
                Span::styled("  · branch ", dim),
                Span::styled(branch.clone(), value),
            ]));
        }
        if let Some(context) = &self.context_summary {
            lines.push(Line::from(vec![
                Span::styled("  · context ", dim),
                Span::styled(context.clone(), value),
            ]));
        }
        if let Some(settings) = &self.settings_summary {
            lines.push(Line::from(vec![
                Span::styled("  · settings ", dim),
                Span::styled(settings.clone(), value),
            ]));
        }
        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            "  Type a prompt to begin. Slash commands:".to_string(),
            dim,
        )));
        lines.push(Line::from(vec![
            Span::styled("    /quit, /exit", dim),
            Span::styled("      exit".to_string(), dim),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    /clear", dim),
            Span::styled("     start a new transcript".to_string(), dim),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    /sessions", dim),
            Span::styled("  not wired yet".to_string(), dim),
        ]));
        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            "  nav asks before risky tools (rm -rf, force-push, .env reads).".to_string(),
            dim,
        )));
        lines.push(Line::from(Span::styled(
            "  Pass --approval-policy never to silence, or --sandbox read-only to harden."
                .to_string(),
            dim,
        )));
        lines.push(Line::from(String::new()));
        lines
    }
}
