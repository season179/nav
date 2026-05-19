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
use crate::theme::Theme;

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
    theme: Theme,
    /// Indices into `entries`, ranked by nucleo match score (best first).
    /// Truncated to [`MAX_VISIBLE`] so navigation never exceeds what's drawn.
    matches: Vec<usize>,
    selected: usize,
    completed: bool,
    /// The token (without the leading `@`) that produced the current
    /// `matches` ranking. `set_query` is called on every keystroke via
    /// `reconcile_popups`; remembering the prior query lets us short-circuit
    /// a no-op refresh so navigation keys don't reset `selected` to 0.
    current_query: String,
}

/// Cap rendered rows so the popup never eats the whole screen on a generic
/// `@` query. `matches` is truncated to this cap so the selection index can
/// always reach every entry the user can actually see.
const MAX_VISIBLE: usize = 8;

impl FileMentionPopup {
    pub fn new(entries: Arc<[MentionEntry]>, initial_query: &str, theme: Theme) -> Self {
        let mut popup = Self {
            entries,
            theme,
            matches: Vec::new(),
            selected: 0,
            completed: false,
            current_query: String::new(),
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
    /// is never blank. A no-op call (same query as last time) is a fast
    /// return — otherwise `reconcile_popups` would reset `selected` to 0 on
    /// every keystroke, including Up/Down, hiding navigation entirely.
    pub fn set_query(&mut self, query: &str) {
        if query == self.current_query && !self.matches.is_empty() {
            return;
        }
        self.current_query = query.to_string();
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
        let raw = self.entries[entry_idx].display.clone();
        let quoted = quote_path_if_needed(&raw);
        self.completed = true;
        if !composer.replace_active_at_token(&quoted) {
            return InputResult::Unhandled;
        }
        let path = PathBuf::from(&raw);
        if is_image_extension(&raw) {
            composer.push_pending_image(path);
        } else {
            composer.push_pending_file(path);
        }
        InputResult::Handled
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let bg = Style::default().bg(self.theme.popup_bg);
        Block::default().style(bg).render(area, buf);

        if self.matches.is_empty() {
            let hint = Span::styled(
                "  no files match",
                bg.fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            );
            Paragraph::new(Line::from(hint)).style(bg).render(area, buf);
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

/// Extension-only check for whether a workspace-relative path should be
/// attached as an image vs. a generic file. Cheap (no I/O) — the agent's
/// `build_user_content` does its own load and silently drops anything that
/// fails to read, so a wrong-extension paste degrades to a no-op image
/// rather than corrupting the prompt. Matches the same extension set that
/// nav-core's `encode_image_data_uri` recognizes.
fn is_image_extension(path: &str) -> bool {
    let Some(ext) = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
    else {
        return false;
    };
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
    )
}

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
        let popup = FileMentionPopup::new(entries(&["a.rs", "b.rs", "c.rs"]), "", Theme::default());
        let names: Vec<&str> = popup.matches().iter().map(|e| e.display.as_str()).collect();
        assert_eq!(names, vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn fuzzy_match_ranks_relevant_paths_first() {
        let popup = FileMentionPopup::new(
            entries(&["src/main.rs", "tests/integration.rs", "src/composer.rs"]),
            "composer",
            Theme::default(),
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

    #[test]
    fn down_navigation_persists_through_reconcile_with_same_query() {
        // Down arrow advances `selected`; `BottomPane::reconcile_popups` then
        // calls `set_query` with the same token on every keystroke. Prior to
        // the no-op guard, this reset `selected` to 0 and the user could
        // never move past the first row.
        let mut popup =
            FileMentionPopup::new(entries(&["a.rs", "b.rs", "c.rs"]), "", Theme::default());
        assert_eq!(popup.selected, 0);
        popup.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
            &mut Composer::new(),
        );
        assert_eq!(popup.selected, 1);
        // Reconcile: same query → must be a no-op for selection.
        popup.set_query("");
        assert_eq!(popup.selected, 1);
    }

    #[test]
    fn completing_non_image_path_queues_file_attachment() {
        let mut popup =
            FileMentionPopup::new(entries(&["src/main.rs", "README.md"]), "", Theme::default());
        let mut composer = Composer::new();
        composer.insert_paste("@");
        popup.set_query("src/main");
        popup.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
            &mut composer,
        );
        assert!(composer.text().contains("src/main.rs"));
        let (_, attachments) = drain_submit(&mut composer);
        assert_eq!(
            attachments,
            vec![nav_core::UserAttachment::File {
                path: PathBuf::from("src/main.rs")
            }]
        );
    }

    #[test]
    fn completing_image_path_queues_image_attachment() {
        let mut popup = FileMentionPopup::new(entries(&["assets/cat.png"]), "", Theme::default());
        let mut composer = Composer::new();
        composer.insert_paste("@");
        popup.set_query("cat");
        popup.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
            &mut composer,
        );
        let (_, attachments) = drain_submit(&mut composer);
        assert_eq!(
            attachments,
            vec![nav_core::UserAttachment::Image {
                path: PathBuf::from("assets/cat.png")
            }]
        );
    }

    fn drain_submit(c: &mut Composer) -> (String, Vec<nav_core::UserAttachment>) {
        let event = c.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        match event {
            super::super::composer::ComposerEvent::Submit { text, attachments } => {
                (text, attachments)
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn is_image_extension_matches_known_image_suffixes() {
        assert!(is_image_extension("a.png"));
        assert!(is_image_extension("path/to/IMG.JPG"));
        assert!(is_image_extension("clip.webp"));
        assert!(!is_image_extension("src/main.rs"));
        assert!(!is_image_extension("README.md"));
        assert!(!is_image_extension("noext"));
    }

    #[test]
    fn changing_query_resets_selection() {
        // Real query changes still reset to 0 so the user lands on the top
        // ranked result for the new pattern.
        let mut popup =
            FileMentionPopup::new(entries(&["alpha", "beta", "gamma"]), "", Theme::default());
        popup.handle_key(
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
            &mut Composer::new(),
        );
        assert_eq!(popup.selected, 1);
        popup.set_query("ga");
        assert_eq!(popup.selected, 0);
    }

    #[test]
    fn build_mention_entries_indexes_hidden_launch_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join(".hidden-worktree");
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        std::fs::write(cwd.join("src").join("main.rs"), "fn main() {}\n").unwrap();

        let entries = build_mention_entries(&cwd);
        let names: Vec<&str> = entries.iter().map(|entry| entry.display.as_str()).collect();

        assert!(names.contains(&"src/main.rs"), "{names:?}");
    }
}
