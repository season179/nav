use nav_core::PendingInputMode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use crate::theme::Theme;

/// Maximum number of items to show before the `+N more` overflow indicator.
pub(super) const VISIBLE_CAP: usize = 4;

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
    let hint = bg.fg(Color::Rgb(120, 120, 140));

    // --- Header row ---
    let count = items.len();
    let extra = count.saturating_sub(VISIBLE_CAP);
    let mut header_spans = vec![
        Span::styled(format!("  {count} pending"), accent),
    ];
    if extra > 0 {
        header_spans.push(Span::styled(
            format!("  +{extra} more"),
            bg.fg(Color::Yellow),
        ));
    }
    header_spans.push(Span::styled("  edit · remove · clear", hint));

    let mut lines = vec![Line::from(header_spans)];

    // --- Item rows ---
    for item in items.iter().take(VISIBLE_CAP) {
        let mode_style = mode_style(item.mode, bg);
        lines.push(Line::from(vec![
            Span::styled("    ", bg),
            Span::styled(format!("{} ", mode_badge(item.mode)), mode_style),
            Span::styled(truncate_preview(&item.text), bg.fg(Color::White)),
            Span::styled(
                format!("  edit:{}  rm:{}", item.id, item.id),
                dim,
            ),
        ]));
    }

    Paragraph::new(lines).style(bg).render(area, buf);
}

/// Mode badge: short label shown before each item's text.
fn mode_badge(mode: PendingInputMode) -> &'static str {
    match mode {
        PendingInputMode::FollowUp => "follow-up",
        PendingInputMode::Steering => "steering",
    }
}

/// Mode-specific colouring. Steering items use a warm amber to visually
/// distinguish them from the cooler blue of follow-up items.
fn mode_style(mode: PendingInputMode, bg: Style) -> Style {
    match mode {
        PendingInputMode::FollowUp => bg.fg(Color::Cyan).add_modifier(Modifier::BOLD),
        PendingInputMode::Steering => bg.fg(Color::Rgb(230, 160, 60)).add_modifier(Modifier::BOLD),
    }
}

/// Truncate to the first line of text, with an ellipsis at the last word
/// boundary that fits within [`MAX_WIDTH`] characters.
fn truncate_preview(text: &str) -> String {
    const MAX_WIDTH: usize = 50;
    // Take only the first line.
    let first_line = text.lines().next().unwrap_or("");
    let first_line = first_line.trim_end();
    if first_line.is_empty() {
        return "(empty)".to_string();
    }
    if first_line.chars().count() <= MAX_WIDTH {
        return first_line.to_string();
    }
    // Walk backwards to find a word boundary.
    let chars: Vec<char> = first_line.chars().take(MAX_WIDTH).collect();
    let mut cut = chars.len();
    while cut > 0 && !chars[cut - 1].is_whitespace() {
        cut -= 1;
    }
    // If no word boundary was found, hard-cut at the char limit.
    if cut == 0 {
        cut = chars.len();
    }
    // Trim trailing whitespace at the cut point.
    while cut > 0 && chars[cut - 1].is_whitespace() {
        cut -= 1;
    }
    let mut out: String = chars[..cut].iter().collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_text_unchanged() {
        assert_eq!(truncate_preview("hello world"), "hello world");
    }

    #[test]
    fn truncate_first_line_only() {
        assert_eq!(
            truncate_preview("short\nsecond line that is very long indeed"),
            "short"
        );
    }

    #[test]
    fn truncate_at_word_boundary() {
        let long = "this is a moderately long prompt that should be truncated at a word boundary";
        let result = truncate_preview(long);
        assert!(result.ends_with('…'), "should end with ellipsis: {result}");
        // Should not end with a space before the ellipsis.
        let trimmed = result.trim_end_matches('…');
        assert!(
            !trimmed.ends_with(' '),
            "should not have trailing space before ellipsis: {trimmed}"
        );
    }

    #[test]
    fn truncate_hard_cut_when_no_spaces() {
        let long = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let result = truncate_preview(long);
        assert!(result.ends_with('…'), "should end with ellipsis: {result}");
        // 50 chars hard-cut + 1 ellipsis char
        assert_eq!(result.chars().count(), 51);
    }

    #[test]
    fn truncate_empty_string_shows_placeholder() {
        assert_eq!(truncate_preview(""), "(empty)");
    }

    #[test]
    fn truncate_whitespace_only_shows_placeholder() {
        assert_eq!(truncate_preview("   \n  "), "(empty)");
    }

    #[test]
    fn truncate_exact_max_width_passes_through() {
        let exact: String = "a".repeat(50);
        let result = truncate_preview(&exact);
        assert_eq!(result, exact);
        assert!(!result.contains('…'), "exact-width text should not be truncated");
    }
}
