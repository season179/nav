use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::composer::Composer;
use super::list_picker::{ListPicker, ListPickerItem};
use super::view::{BottomPaneView, InputResult};
use crate::theme::Theme;

/// One row in the $skill picker popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
}

impl ListPickerItem for SkillEntry {
    fn render_line(&self, selected: bool, theme: &Theme) -> Line<'static> {
        let bg = Style::default().bg(theme.popup_bg);
        let row_style = if selected {
            bg.fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            bg.fg(Color::Gray)
        };
        let mut spans = vec![
            Span::styled("  ", row_style),
            Span::styled(self.name.clone(), row_style),
        ];
        if !self.description.is_empty() {
            spans.push(Span::styled("  ", row_style));
            spans.push(Span::styled(
                self.description.clone(),
                bg.fg(Color::DarkGray),
            ));
        }
        Line::from(spans)
    }
}

/// Overlay that pops up while the cursor sits inside a `$token`. Prefix-
/// filters available skills and lets the user pick one with Enter / Tab.
/// Navigation and rendering are delegated to [`ListPicker<SkillEntry>`];
/// this wrapper owns the filter logic and the Enter / Tab submit behaviour.
pub struct SkillPopup {
    entries: Arc<[SkillEntry]>,
    /// Indices into `entries` that match the current filter, ranked by
    /// prefix-match priority. Capped to [`MAX_VISIBLE`].
    match_indices: Vec<usize>,
    /// The token (without the leading `$`) that produced the current
    /// `match_indices`. Cached so `set_query` can short-circuit a no-op
    /// refresh when `reconcile_popups` fires on navigation keys.
    current_query: String,
    picker: ListPicker<SkillEntry>,
}

/// Maximum skill suggestions shown at once.
const MAX_VISIBLE: usize = 8;

impl SkillPopup {
    pub fn new(entries: Arc<[SkillEntry]>, initial_query: &str, theme: Theme) -> Self {
        let mut popup = Self {
            entries,
            match_indices: Vec::new(),
            current_query: String::new(),
            picker: ListPicker::new(Vec::new(), MAX_VISIBLE, theme),
        };
        popup.set_query(initial_query);
        popup
    }

    pub fn matches(&self) -> Vec<&SkillEntry> {
        self.match_indices
            .iter()
            .map(|&i| &self.entries[i])
            .collect()
    }

    /// Re-rank `entries` against `query` using prefix matching. An empty
    /// query surfaces all entries (capped to [`MAX_VISIBLE`]). A no-op call
    /// (same query as last time) is a fast return so navigation keys don't
    /// reset `selected` to 0.
    pub fn set_query(&mut self, query: &str) {
        if query == self.current_query && !self.match_indices.is_empty() {
            return;
        }
        self.current_query = query.to_string();
        self.match_indices.clear();
        if self.entries.is_empty() {
            return;
        }

        let query_lower = query.to_lowercase();

        // Collect matching indices, with prefix matches ranked first.
        let mut scored: Vec<(bool, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                let name_lower = entry.name.to_lowercase();
                if query.is_empty() || name_lower.contains(&query_lower) {
                    let prefix = name_lower.starts_with(&query_lower);
                    Some((prefix, idx))
                } else {
                    None
                }
            })
            .collect();

        // Prefix matches first, then by name for stable ordering.
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| self.entries[a.1].name.cmp(&self.entries[b.1].name))
        });

        self.match_indices
            .extend(scored.into_iter().take(MAX_VISIBLE).map(|(_, idx)| idx));

        // Push the filtered items into the picker so it can render and track
        // selection.
        let filtered: Vec<SkillEntry> = self
            .match_indices
            .iter()
            .map(|&idx| self.entries[idx].clone())
            .collect();
        self.picker.set_items(filtered);
    }

    fn handle_key_inner(&mut self, key: KeyEvent, composer: &mut Composer) -> InputResult {
        if key.kind == KeyEventKind::Release {
            return InputResult::Unhandled;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Tab, _) => self.try_complete(composer),
            (KeyCode::Enter, m) if !m.contains(KeyModifiers::SHIFT) => {
                self.try_complete(composer)
            }
            _ => self.picker.handle_navigation_key(key),
        }
    }

    fn try_complete(&mut self, composer: &mut Composer) -> InputResult {
        let Some(entry) = self.picker.selected_item() else {
            return InputResult::Unhandled;
        };
        let name = entry.name.clone();
        if !composer.replace_active_dollar_token(&name) {
            return InputResult::Unhandled;
        }
        self.picker.confirm();
        InputResult::Handled
    }
}

impl BottomPaneView for SkillPopup {
    fn handle_key(&mut self, key: KeyEvent, composer: &mut Composer) -> InputResult {
        self.handle_key_inner(key, composer)
    }

    fn is_complete(&self) -> bool {
        self.picker.is_complete()
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.picker.desired_height(width)
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.picker.render(area, buf);
    }

    fn is_text_driven(&self) -> bool {
        true
    }
}

/// Build the combined skill entry list shared by every skill popup instance,
/// sourced from the skill catalog.
pub fn build_skill_entries(skills: &nav_core::Catalog) -> Arc<[SkillEntry]> {
    skills
        .iter()
        .map(|skill| SkillEntry {
            name: skill.name.clone(),
            description: skill.description.clone(),
        })
        .collect::<Vec<_>>()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nav_core::{Catalog, Skill, SkillScope};

    fn make_catalog() -> Catalog {
        Catalog::new(vec![
            Skill {
                name: "code-reviewer".into(),
                description: "review code for quality".into(),
                skill_md_path: "/tmp/code-reviewer/SKILL.md".into(),
                skill_dir: "/tmp/code-reviewer".into(),
                scope: SkillScope::Project,
            },
            Skill {
                name: "caveman".into(),
                description: "ultra-compressed communication".into(),
                skill_md_path: "/tmp/caveman/SKILL.md".into(),
                skill_dir: "/tmp/caveman".into(),
                scope: SkillScope::Project,
            },
            Skill {
                name: "diagnose".into(),
                description: "disciplined diagnosis loop".into(),
                skill_md_path: "/tmp/diagnose/SKILL.md".into(),
                skill_dir: "/tmp/diagnose".into(),
                scope: SkillScope::Project,
            },
            Skill {
                name: "tdd".into(),
                description: "test-driven development".into(),
                skill_md_path: "/tmp/tdd/SKILL.md".into(),
                skill_dir: "/tmp/tdd".into(),
                scope: SkillScope::Project,
            },
        ])
    }

    fn entries(catalog: &Catalog) -> Arc<[SkillEntry]> {
        build_skill_entries(catalog)
    }

    #[test]
    fn empty_query_surfaces_all_skills() {
        let catalog = make_catalog();
        let popup = SkillPopup::new(entries(&catalog), "", Theme::default());
        let names: Vec<&str> = popup.matches().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names.len(), 4);
        assert!(names.contains(&"caveman"));
        assert!(names.contains(&"code-reviewer"));
        assert!(names.contains(&"diagnose"));
        assert!(names.contains(&"tdd"));
    }

    #[test]
    fn set_query_prefix_matches_rank_first() {
        let catalog = make_catalog();
        let popup = SkillPopup::new(entries(&catalog), "code", Theme::default());
        let first = popup.matches().first().map(|e| e.name.as_str()).unwrap_or("");
        assert_eq!(first, "code-reviewer");
    }

    #[test]
    fn set_query_substring_match_falls_back() {
        let catalog = make_catalog();
        let popup = SkillPopup::new(entries(&catalog), "dia", Theme::default());
        let names: Vec<&str> = popup.matches().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["diagnose"]);
    }

    #[test]
    fn set_query_no_match_shows_empty() {
        let catalog = make_catalog();
        let popup = SkillPopup::new(entries(&catalog), "xyz", Theme::default());
        assert!(popup.matches().is_empty());
    }

    #[test]
    fn same_query_is_noop_for_selection() {
        // Down arrow advances `selected`; `reconcile_popups` then calls
        // `set_query` with the same token on every keystroke. Without the
        // no-op guard this would reset selection to 0.
        let catalog = make_catalog();
        let mut popup = SkillPopup::new(entries(&catalog), "", Theme::default());
        assert_eq!(popup.picker.selected_index(), 0);
        popup.picker.move_down();
        assert_eq!(popup.picker.selected_index(), 1);
        popup.set_query("");
        assert_eq!(popup.picker.selected_index(), 1);
    }

    #[test]
    fn changing_query_resets_selection() {
        let catalog = make_catalog();
        let mut popup = SkillPopup::new(entries(&catalog), "", Theme::default());
        popup.picker.move_down();
        assert_eq!(popup.picker.selected_index(), 1);
        popup.set_query("dia");
        assert_eq!(popup.picker.selected_index(), 0);
    }

    #[test]
    fn enter_selects_and_replaces_dollar_token() {
        let catalog = make_catalog();
        let mut popup = SkillPopup::new(entries(&catalog), "code", Theme::default());
        let mut composer = Composer::new();
        composer.set_text("$code");
        popup.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
            &mut composer,
        );
        assert!(popup.is_complete());
        assert_eq!(composer.text(), "code-reviewer ");
    }

    #[test]
    fn tab_selects_and_replaces_dollar_token() {
        let catalog = make_catalog();
        let mut popup = SkillPopup::new(entries(&catalog), "code", Theme::default());
        let mut composer = Composer::new();
        composer.set_text("$code");
        popup.handle_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
            &mut composer,
        );
        assert!(popup.is_complete());
        assert_eq!(composer.text(), "code-reviewer ");
    }

    #[test]
    fn esc_dismisses_without_replacement() {
        let catalog = make_catalog();
        let mut popup = SkillPopup::new(entries(&catalog), "code", Theme::default());
        let mut composer = Composer::new();
        composer.set_text("$code");
        popup.handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
            &mut composer,
        );
        assert!(popup.is_complete());
        assert_eq!(composer.text(), "$code");
    }

    #[test]
    fn case_insensitive_filtering() {
        let catalog = make_catalog();
        let popup = SkillPopup::new(entries(&catalog), "CODE", Theme::default());
        let first = popup.matches().first().map(|e| e.name.as_str()).unwrap_or("");
        assert_eq!(first, "code-reviewer");
    }
}
