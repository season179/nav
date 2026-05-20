use nav_core::PendingInputMode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use crate::theme::Theme;

#[derive(Debug, Clone)]
pub(super) struct PendingPreview {
    pub(super) id: String,
    pub(super) mode: PendingInputMode,
    pub(super) text: String,
}

pub(super) fn render_pending_preview(
    items: &[PendingPreview],
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    let bg = Style::default().bg(theme.composer_bg);
    Block::default().style(bg).render(area, buf);
    let dim = bg.fg(Color::DarkGray);
    let accent = bg.fg(Color::Blue).add_modifier(Modifier::BOLD);
    let mut lines = vec![Line::from(vec![
        Span::styled("  pending", accent),
        Span::styled(
            "  edit: /queue-edit <id> ...  remove: /queue-remove <id>  clear: /queue-clear",
            dim,
        ),
    ])];
    for item in items.iter().take(4) {
        lines.push(Line::from(vec![
            Span::styled("  • ", dim),
            Span::styled(item.id.clone(), accent),
            Span::styled(format!(" {}  ", pending_mode_label(item.mode)), dim),
            Span::styled(truncate_preview(&item.text), bg.fg(Color::White)),
        ]));
    }
    Paragraph::new(lines).style(bg).render(area, buf);
}

fn pending_mode_label(mode: PendingInputMode) -> &'static str {
    match mode {
        PendingInputMode::FollowUp => "follow-up",
        PendingInputMode::Steering => "steering",
    }
}

fn truncate_preview(text: &str) -> String {
    const MAX_CHARS: usize = 80;
    if text.chars().count() <= MAX_CHARS {
        return text.to_string();
    }
    let mut out = text.chars().take(MAX_CHARS).collect::<String>();
    out.push('…');
    out
}
