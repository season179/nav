use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::composer::Composer;
use super::view::InputResult;

/// One row in the @file mention popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionEntry {
    /// Workspace-relative path, the literal text that gets inserted.
    pub display: String,
}

impl MentionEntry {
    pub fn from_relative(path: PathBuf) -> Self {
        Self {
            display: path.to_string_lossy().into_owned(),
        }
    }
}

/// Overlay that pops up while the cursor sits inside an `@token`. Fuzzy
/// matches the token against the workspace file index built once at TUI
/// startup; selecting an entry replaces the `@token` with the literal path
/// (no `@` prefix, quoted if it has whitespace) plus a trailing space —
/// matches codex `chat_composer.rs::insert_selected_path` semantics.
pub struct FileMentionPopup {
    entries: Arc<[MentionEntry]>,
    /// Indices into `entries`, ranked by nucleo match score (best first).
    /// Truncated to [`MAX_VISIBLE`] so navigation never exceeds what's drawn.
    matches: Vec<usize>,
    selected: usize,
    completed: bool,
}

/// Cap rendered rows so the popup never eats the whole screen on a generic
/// `@` query. `matches` is truncated to this cap so the selection index can
/// always reach every entry the user can actually see.
const MAX_VISIBLE: usize = 8;

impl FileMentionPopup {
    pub fn new(entries: Arc<[MentionEntry]>, initial_query: &str) -> Self {
        let mut popup = Self {
            entries,
            matches: Vec::new(),
            selected: 0,
            completed: false,
        };
        popup.set_query(initial_query);
        popup
    }

    pub fn matches(&self) -> Vec<&MentionEntry> {
        self.matches.iter().map(|&i| &self.entries[i]).collect()
    }

    pub fn is_complete(&self) -> bool {
        self.completed
    }

    pub fn desired_height(&self, _width: u16) -> u16 {
        self.matches.len().clamp(1, MAX_VISIBLE) as u16
    }

    /// Re-rank `entries` against `query` using `nucleo-matcher` and keep the
    /// top [`MAX_VISIBLE`] scores. Empty query surfaces a prefix so the popup
    /// is never blank.
    pub fn set_query(&mut self, query: &str) {
        self.matches.clear();
        if self.entries.is_empty() {
            self.selected = 0;
            return;
        }

        if query.is_empty() {
            for i in 0..self.entries.len().min(MAX_VISIBLE) {
                self.matches.push(i);
            }
            self.selected = 0;
            return;
        }

        let mut matcher = Matcher::default();
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = Vec::new();
        for (idx, entry) in self.entries.iter().enumerate() {
            buf.clear();
            let haystack = Utf32Str::new(&entry.display, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                scored.push((score, idx));
            }
        }
        // Highest score first, then shorter paths first as a tiebreaker so an
        // exact filename match outranks a same-score deeper path.
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| {
                self.entries[a.1]
                    .display
                    .len()
                    .cmp(&self.entries[b.1].display.len())
            })
        });
        for (_, idx) in scored.into_iter().take(MAX_VISIBLE) {
            self.matches.push(idx);
        }
        self.selected = 0;
    }

    pub fn handle_key(&mut self, key: KeyEvent, composer: &mut Composer) -> InputResult {
        if key.kind == KeyEventKind::Release {
            return InputResult::Unhandled;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Tab, _) => self.try_complete(composer),
            (KeyCode::Enter, m) if !m.contains(KeyModifiers::SHIFT) => self.try_complete(composer),
            (KeyCode::Up, _) => {
                self.selected = self.selected.saturating_sub(1);
                InputResult::Handled
            }
            (KeyCode::Down, _) => {
                if self.selected + 1 < self.matches.len() {
                    self.selected += 1;
                }
                InputResult::Handled
            }
            (KeyCode::Esc, _) => {
                self.completed = true;
                InputResult::Handled
            }
            _ => InputResult::Unhandled,
        }
    }

    fn try_complete(&mut self, composer: &mut Composer) -> InputResult {
        let Some(&entry_idx) = self.matches.get(self.selected) else {
            return InputResult::Unhandled;
        };
        let path = quote_path_if_needed(&self.entries[entry_idx].display);
        self.completed = true;
        if composer.replace_active_at_token(&path) {
            InputResult::Handled
        } else {
            InputResult::Unhandled
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let bg = Style::default().bg(crate::theme::POPUP_BG);
        Block::default().style(bg).render(area, buf);

        if self.matches.is_empty() {
            let hint = Span::styled(
                "  no files match",
                bg.fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            );
            Paragraph::new(Line::from(hint))
                .style(bg)
                .render(area, buf);
            return;
        }

        let lines: Vec<Line<'_>> = self
            .matches
            .iter()
            .enumerate()
            .map(|(row, &entry_idx)| {
                let entry = &self.entries[entry_idx];
                let row_style = if row == self.selected {
                    bg.fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    bg.fg(Color::Gray)
                };
                Line::from(vec![
                    Span::styled("  ", row_style),
                    Span::styled(entry.display.as_str(), row_style),
                ])
            })
            .collect();
        Paragraph::new(lines).style(bg).render(area, buf);
    }
}

/// Walk the workspace once, respecting `.gitignore`, and collect up to
/// [`MENTION_INDEX_CAP`] workspace-relative file paths. Result is shared via
/// `Arc` so the popup widget is cheap to construct on every keystroke.
pub fn build_mention_entries(cwd: &Path) -> Arc<[MentionEntry]> {
    let mut entries: Vec<MentionEntry> = Vec::new();
    let walker = ignore::WalkBuilder::new(cwd).build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(cwd) else {
            continue;
        };
        entries.push(MentionEntry::from_relative(rel.to_path_buf()));
        if entries.len() >= MENTION_INDEX_CAP {
            break;
        }
    }
    entries.into()
}

/// Cap on indexed paths. Beyond this the popup risks scanning more text than
/// the user can usefully filter through. Same order of magnitude as codex's
/// `file-search` defaults.
const MENTION_INDEX_CAP: usize = 20_000;

/// Quote a path the way a shell would: if it contains whitespace or shell
/// metacharacters, wrap it in single quotes and escape any embedded single
/// quotes. Keeps the inserted token unambiguous when the agent or the user
/// later splits the buffer on whitespace. Mirrors codex's quoting in
/// `insert_selected_path`.
pub fn quote_path_if_needed(path: &str) -> String {
    let needs_quote = path
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '$' | '`' | '#'));
    if !needs_quote {
        return path.to_string();
    }
    let escaped = path.replace('\'', "'\\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(paths: &[&str]) -> Arc<[MentionEntry]> {
        paths
            .iter()
            .map(|p| MentionEntry {
                display: (*p).to_string(),
            })
            .collect::<Vec<_>>()
            .into()
    }

    #[test]
    fn empty_query_surfaces_prefix() {
        let popup = FileMentionPopup::new(entries(&["a.rs", "b.rs", "c.rs"]), "");
        let names: Vec<&str> = popup.matches().iter().map(|e| e.display.as_str()).collect();
        assert_eq!(names, vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn fuzzy_match_ranks_relevant_paths_first() {
        let popup = FileMentionPopup::new(
            entries(&["src/main.rs", "tests/integration.rs", "src/composer.rs"]),
            "composer",
        );
        let first = popup
            .matches()
            .first()
            .map(|e| e.display.as_str())
            .unwrap_or("");
        assert_eq!(first, "src/composer.rs");
    }

    #[test]
    fn quote_path_skips_safe_paths() {
        assert_eq!(quote_path_if_needed("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn quote_path_wraps_paths_with_spaces() {
        assert_eq!(
            quote_path_if_needed("My Documents/notes.txt"),
            "'My Documents/notes.txt'"
        );
    }

    #[test]
    fn quote_path_escapes_embedded_single_quote() {
        assert_eq!(quote_path_if_needed("don't.txt"), "'don'\\''t.txt'");
    }
}
