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
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let dim = Style::default().fg(Color::DarkGray);
        let value = Style::default().fg(Color::White);
        let accent = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let session_short: String = self.session_id.chars().take(10).collect();
        let mut rows = vec![
            ("model", self.model.clone()),
            ("cwd", self.cwd.clone()),
            ("session", session_short),
        ];
        if let Some(branch) = &self.branch_summary {
            rows.push(("branch", branch.clone()));
        }
        if let Some(context) = &self.context_summary {
            rows.push(("context", context.clone()));
        }
        if let Some(settings) = &self.settings_summary {
            rows.push(("settings", settings.clone()));
        }
        let mut lines = bordered_session_card("nav", &rows, width, dim, value, accent);
        lines.push(Line::from(String::new()));
        lines.push(dim_line(
            "  Type a prompt. /quit exits, /clear resets, /sessions resumes.",
            width,
            dim,
        ));
        lines.push(dim_line(
            "  nav asks before risky tools; --sandbox read-only hardens the run.",
            width,
            dim,
        ));
        lines.push(Line::from(String::new()));
        lines
    }
}

fn bordered_session_card(
    title: &str,
    rows: &[(&str, String)],
    width: u16,
    dim: Style,
    value: Style,
    accent: Style,
) -> Vec<Line<'static>> {
    let width = width as usize;
    if width < 12 {
        let mut lines = vec![Line::from(Span::styled(
            truncate_to_width(title, width),
            accent,
        ))];
        for (label, row_value) in rows {
            lines.push(dim_line(
                &format!("{label}: {row_value}"),
                width as u16,
                dim,
            ));
        }
        return lines;
    }

    let inner_width = width.saturating_sub(4);
    let title_text = format!("╭─ {title} ");
    let top = fill_to_width(title_text, "─", "╮", width);
    let bottom = fill_to_width("╰".to_string(), "─", "╯", width);
    let mut lines = vec![Line::from(vec![Span::styled(top, accent)])];
    for (label, row_value) in rows {
        let label_text = format!("{label}: ");
        let value_width = inner_width.saturating_sub(label_text.chars().count());
        let clipped = truncate_to_width(row_value, value_width);
        let content_width = label_text.chars().count() + clipped.chars().count();
        let padding = inner_width.saturating_sub(content_width);
        lines.push(Line::from(vec![
            Span::styled("│ ".to_string(), dim),
            Span::styled(label_text, dim),
            Span::styled(clipped, value),
            Span::styled(format!("{} │", " ".repeat(padding)), dim),
        ]));
    }
    lines.push(Line::from(Span::styled(bottom, dim)));
    lines
}

fn fill_to_width(prefix: String, fill: &str, suffix: &str, width: usize) -> String {
    let fixed = prefix.chars().count() + suffix.chars().count();
    if fixed >= width {
        return truncate_to_width(format!("{prefix}{suffix}"), width);
    }
    format!("{prefix}{}{suffix}", fill.repeat(width - fixed))
}

fn dim_line(text: &str, width: u16, style: Style) -> Line<'static> {
    Line::from(Span::styled(truncate_to_width(text, width as usize), style))
}

fn truncate_to_width(text: impl AsRef<str>, width: usize) -> String {
    let text = text.as_ref();
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out = text.chars().take(width - 1).collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn welcome_renders_compact_bordered_session_card() {
        let cell = WelcomeCell::new(
            "gpt-5.4",
            "/Users/season/Personal/nav",
            "01HZZZZZZZZZZZZZZZZZZZZZZZ",
            Some("main".to_string()),
            Some("AGENTS.md (project)".to_string()),
            Some(".nav/settings.json".to_string()),
        );
        let lines = cell.display_lines(48);

        assert_eq!(
            line_text(&lines[0]),
            "╭─ nav ────────────────────────────────────────╮"
        );
        assert_eq!(
            line_text(&lines[1]),
            "│ model: gpt-5.4                               │"
        );
        assert_eq!(
            line_text(&lines[2]),
            "│ cwd: /Users/season/Personal/nav              │"
        );
        assert!(
            lines
                .iter()
                .all(|line| line_text(line).chars().count() <= 48)
        );
    }

    #[test]
    fn welcome_clips_narrow_rows() {
        let cell = WelcomeCell::new(
            "very-long-model-name",
            "/a/very/long/path",
            "01HZZZZZZZZZZZZZZZZZZZZZZZ",
            None,
            None,
            None,
        );
        let lines = cell.display_lines(20);

        assert!(
            lines
                .iter()
                .all(|line| line_text(line).chars().count() <= 20)
        );
    }
}
