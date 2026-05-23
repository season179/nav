//! Ctrl+R incremental history search overlay.
//!
//! Filters the composer's submitted-prompt history by substring and lets the
//! user pick a match with Up/Down + Enter. Esc restores the pre-search buffer.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::composer::Composer;
use super::view::{BottomPaneView, InputResult};
use crate::theme::Theme;

/// Maximum number of matching history entries rendered in the popup.
const MAX_VISIBLE: usize = 6;

/// Overlay that incrementally searches the composer's prompt history.
pub struct HistorySearch {
    theme: Theme,
    query: String,
    history: Vec<String>,
    matches: Vec<usize>,
    selected: usize,
    completed: bool,
    pre_search_buffer: String,
}

impl HistorySearch {
    /// Create a new search overlay. `history` is a snapshot of the composer's
    /// submitted-prompt list; `initial_query` seeds the filter (typically the
    /// text the user had typed so far).
    pub fn new(history: Vec<String>, initial_query: &str, theme: Theme) -> Self {
        let mut search = Self {
            theme,
            query: initial_query.to_string(),
            history,
            matches: Vec::new(),
            selected: 0,
            completed: false,
            pre_search_buffer: initial_query.to_string(),
        };
        search.refilter();
        search
    }

    /// Rebuild the match list newest-first, capped at [`MAX_VISIBLE`].
    fn refilter(&mut self) {
        self.matches.clear();
        // Walk history newest-first so the most recent match appears first.
        // Cap at MAX_VISIBLE so navigation can never reach off-screen entries.
        for idx in (0..self.history.len()).rev() {
            if self.query.is_empty() || self.history[idx].contains(&self.query) {
                self.matches.push(idx);
            }
            if self.matches.len() >= MAX_VISIBLE {
                break;
            }
        }
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }

    /// Route a key while the search overlay is active. All keys are
    /// swallowed; Enter selects, Esc restores, Up/Down navigates,
    /// Ctrl+R cycles, and printable characters append to the query.
    fn handle_key_inner(&mut self, key: KeyEvent, composer: &mut Composer) -> InputResult {
        if key.kind == KeyEventKind::Release {
            return InputResult::Handled;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                self.completed = true;
                composer.set_text(&self.pre_search_buffer);
                InputResult::Handled
            }
            (KeyCode::Enter, _) => {
                if let Some(&idx) = self.matches.get(self.selected) {
                    composer.set_text(&self.history[idx]);
                } else {
                    // No matches — restore the pre-search buffer so the
                    // composer isn't left with stale text.
                    composer.set_text(&self.pre_search_buffer);
                }
                self.completed = true;
                InputResult::Handled
            }
            (KeyCode::Up, _) => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                InputResult::Handled
            }
            (KeyCode::Down, _) => {
                if self.selected + 1 < self.matches.len() {
                    self.selected += 1;
                }
                InputResult::Handled
            }
            // Ctrl+R cycles to the next (older) match, like bash.
            (KeyCode::Char('r'), m) if m.contains(KeyModifiers::CONTROL) => {
                if !self.matches.is_empty() && self.selected + 1 < self.matches.len() {
                    self.selected += 1;
                }
                InputResult::Handled
            }
            (KeyCode::Backspace, _) => {
                if !self.query.is_empty() {
                    // Drop last char (handles multibyte correctly).
                    let new_len = self.query.chars().count().saturating_sub(1);
                    self.query = self.query.chars().take(new_len).collect();
                    self.refilter();
                }
                InputResult::Handled
            }
            (KeyCode::Char(c), m) => {
                // Ignore Ctrl/Alt-modified chars (except Ctrl+R above).
                if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) {
                    return InputResult::Handled;
                }
                self.query.push(c);
                self.refilter();
                InputResult::Handled
            }
            // Swallow everything else so it doesn't leak to the composer.
            _ => InputResult::Handled,
        }
    }

    /// Paint the search prompt, matching entries, and optional "no
    /// matches" hint into the popup area.
    fn render_inner(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let bg = Style::default().bg(self.theme.popup_bg);
        Block::default().style(bg).render(area, buf);

        let mut lines: Vec<Line<'_>> = Vec::new();

        // Search prompt line.
        let prompt_style = bg.fg(Color::Cyan).add_modifier(Modifier::BOLD);
        let query_style = bg.fg(Color::White);
        lines.push(Line::from(vec![
            Span::styled("bck-i-search: ", prompt_style),
            Span::styled(self.query.as_str(), query_style),
        ]));

        // Matching entries (already capped to MAX_VISIBLE by refilter).
        for (row, &hist_idx) in self.matches.iter().enumerate() {
            let is_selected = row == self.selected;
            let entry_style = if is_selected {
                bg.fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                bg.fg(Color::Gray)
            };
            let entry = &self.history[hist_idx];
            // Truncate to fit width minus gutter.
            let max_chars = area.width.saturating_sub(4) as usize;
            let display: String = entry.chars().take(max_chars).collect();
            let marker = if is_selected { " > " } else { "   " };
            lines.push(Line::from(vec![
                Span::styled(marker, entry_style),
                Span::styled(display, entry_style),
            ]));
        }

        // If no matches, show a hint.
        if self.matches.is_empty() {
            let hint_style = bg.fg(Color::DarkGray);
            lines.push(Line::from(Span::styled("  (no matches)", hint_style)));
        }

        Paragraph::new(lines).style(bg).render(area, buf);
    }
}

impl BottomPaneView for HistorySearch {
    fn handle_key(&mut self, key: KeyEvent, composer: &mut Composer) -> InputResult {
        self.handle_key_inner(key, composer)
    }

    fn is_complete(&self) -> bool {
        self.completed
    }

    fn desired_height(&self, _width: u16) -> u16 {
        // 1 for the search prompt + match rows (or "no matches" hint).
        let match_rows = self.matches.len().max(1);
        1 + match_rows as u16
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_inner(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::default()
    }

    fn history() -> Vec<String> {
        vec![
            "hello world".into(),
            "fix the bug".into(),
            "run tests".into(),
            "fix the tests".into(),
        ]
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn empty_query_lists_all_history_newest_first() {
        let search = HistorySearch::new(history(), "", theme());
        // Capped to MAX_VISIBLE even though all 4 entries match.
        assert_eq!(search.matches.len(), 4);
        assert_eq!(search.history[search.matches[0]], "fix the tests");
    }

    #[test]
    fn query_filters_by_substring() {
        let search = HistorySearch::new(history(), "fix", theme());
        assert_eq!(search.matches.len(), 2);
        // Newest first: "fix the tests" then "fix the bug".
        assert_eq!(search.history[search.matches[0]], "fix the tests");
        assert_eq!(search.history[search.matches[1]], "fix the bug");
    }

    #[test]
    fn up_down_navigate_matches() {
        let mut search = HistorySearch::new(history(), "fix", theme());
        assert_eq!(search.selected, 0);

        search.handle_key_inner(key(KeyCode::Down, KeyModifiers::NONE), &mut Composer::new());
        assert_eq!(search.selected, 1);

        // Past the end stays put.
        search.handle_key_inner(key(KeyCode::Down, KeyModifiers::NONE), &mut Composer::new());
        assert_eq!(search.selected, 1);

        search.handle_key_inner(key(KeyCode::Up, KeyModifiers::NONE), &mut Composer::new());
        assert_eq!(search.selected, 0);

        // Before start stays put.
        search.handle_key_inner(key(KeyCode::Up, KeyModifiers::NONE), &mut Composer::new());
        assert_eq!(search.selected, 0);
    }

    #[test]
    fn ctrl_r_cycles_to_next_older_match() {
        let mut search = HistorySearch::new(history(), "fix", theme());
        assert_eq!(search.selected, 0);

        search.handle_key_inner(
            key(KeyCode::Char('r'), KeyModifiers::CONTROL),
            &mut Composer::new(),
        );
        assert_eq!(search.selected, 1);
    }

    #[test]
    fn enter_fills_composer() {
        let mut search = HistorySearch::new(history(), "fix", theme());
        let mut composer = Composer::new();

        search.handle_key_inner(
            key(KeyCode::Down, KeyModifiers::NONE),
            &mut composer,
        );
        search.handle_key_inner(key(KeyCode::Enter, KeyModifiers::NONE), &mut composer);

        assert!(search.completed);
        assert_eq!(composer.text(), "fix the bug");
    }

    #[test]
    fn esc_restores_pre_search_buffer() {
        let mut search = HistorySearch::new(history(), "initial draft", theme());
        let mut composer = Composer::new();

        // Type into search query.
        search.handle_key_inner(
            key(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut composer,
        );
        // Esc restores the pre-search buffer, NOT the query.
        search.handle_key_inner(key(KeyCode::Esc, KeyModifiers::NONE), &mut composer);

        assert!(search.completed);
        assert_eq!(composer.text(), "initial draft");
    }

    #[test]
    fn backspace_removes_query_char_and_refilters() {
        let mut search = HistorySearch::new(history(), "fix the", theme());
        assert_eq!(search.matches.len(), 2); // "fix the bug" and "fix the tests"

        search.handle_key_inner(
            key(KeyCode::Backspace, KeyModifiers::NONE),
            &mut Composer::new(),
        );
        // Query is now "fix th" — still matches both.
        assert_eq!(search.query, "fix th");

        // Backspace to "fix " (trailing space).
        for _ in 0..2 {
            search.handle_key_inner(
                key(KeyCode::Backspace, KeyModifiers::NONE),
                &mut Composer::new(),
            );
        }
        assert_eq!(search.query, "fix ");
        // Now type something that can't match.
        search.handle_key_inner(
            key(KeyCode::Char('z'), KeyModifiers::NONE),
            &mut Composer::new(),
        );
        assert_eq!(search.query, "fix z");
        assert!(search.matches.is_empty());
    }

    #[test]
    fn no_matches_shows_hint() {
        let search = HistorySearch::new(history(), "zzzzz", theme());
        assert!(search.matches.is_empty());
        // desired_height = 1 (prompt) + 1 (hint).
        assert_eq!(search.desired_height(80), 2);
    }

    #[test]
    fn desired_height_caps_at_max_visible() {
        let big_history: Vec<String> = (0..20).map(|i| format!("entry {i}")).collect();
        let search = HistorySearch::new(big_history, "", theme());
        // refilter caps matches to MAX_VISIBLE.
        assert_eq!(search.matches.len(), MAX_VISIBLE);
        assert_eq!(search.desired_height(80), 1 + MAX_VISIBLE as u16);
    }

    #[test]
    fn typing_in_search_appends_to_query() {
        let mut search = HistorySearch::new(history(), "fi", theme());
        search.handle_key_inner(
            key(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut Composer::new(),
        );
        assert_eq!(search.query, "fix");
        assert_eq!(search.matches.len(), 2);
    }

    #[test]
    fn ctrl_and_alt_chars_are_swallowed() {
        let mut search = HistorySearch::new(history(), "", theme());
        search.handle_key_inner(
            key(KeyCode::Char('a'), KeyModifiers::CONTROL),
            &mut Composer::new(),
        );
        assert_eq!(search.query, "");

        search.handle_key_inner(
            key(KeyCode::Char('b'), KeyModifiers::ALT),
            &mut Composer::new(),
        );
        assert_eq!(search.query, "");
    }

    #[test]
    fn enter_with_no_matches_restores_pre_search_buffer() {
        let mut search = HistorySearch::new(history(), "zzzzz", theme());
        let mut composer = Composer::new();

        assert!(search.matches.is_empty());
        search.handle_key_inner(key(KeyCode::Enter, KeyModifiers::NONE), &mut composer);

        assert!(search.completed);
        assert_eq!(composer.text(), "zzzzz");
    }
}
