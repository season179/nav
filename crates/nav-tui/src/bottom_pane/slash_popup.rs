use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::composer::Composer;
use super::view::InputResult;

/// Built-in slash commands the TUI always offers.
pub const BUILTIN_SLASH_COMMANDS: &[&str] = &[
    "/help",
    "/clear",
    "/quit",
    "/exit",
    "/resume",
    "/sessions",
    "/compact",
];

/// One row in the slash popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashEntry {
    pub command: String,
    pub description: Option<String>,
}

impl SlashEntry {
    pub fn builtin(command: &str) -> Self {
        Self {
            command: command.to_string(),
            description: None,
        }
    }
}

/// Overlay that filters [`SlashEntry`] entries by composer-buffer prefix and
/// lets the user pick one with Tab / Enter. Entries are shared via `Arc` so
/// opening the popup is a refcount bump, and matches are tracked as indices
/// into that shared slice — keystroke filtering and the ~12 Hz render path
/// never clone an entry.
pub struct SlashCommandPopup {
    entries: Arc<[SlashEntry]>,
    filter: String,
    matches: Vec<usize>,
    selected: usize,
    completed: bool,
}

impl SlashCommandPopup {
    pub fn new(entries: Arc<[SlashEntry]>) -> Self {
        let matches = (0..entries.len()).collect();
        Self {
            entries,
            filter: String::from("/"),
            matches,
            selected: 0,
            completed: false,
        }
    }

    pub fn matches(&self) -> Vec<&SlashEntry> {
        self.matches.iter().map(|&i| &self.entries[i]).collect()
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn is_complete(&self) -> bool {
        self.completed
    }

    pub fn desired_height(&self, _width: u16) -> u16 {
        self.matches.len().max(1) as u16
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let bg = Style::default().bg(crate::theme::POPUP_BG);
        Block::default().style(bg).render(area, buf);
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
                let mut spans = vec![
                    Span::styled("  ", row_style),
                    Span::styled(entry.command.as_str(), row_style),
                ];
                if let Some(desc) = entry.description.as_deref() {
                    spans.push(Span::styled("  ", row_style));
                    spans.push(Span::styled(desc, bg.fg(Color::DarkGray)));
                }
                Line::from(spans)
            })
            .collect();
        Paragraph::new(lines).style(bg).render(area, buf);
    }

    pub fn on_composer_text_changed(&mut self, first_line: &str) {
        if !first_line.starts_with('/') {
            self.completed = true;
            return;
        }
        if first_line == self.filter {
            return;
        }
        self.filter.clear();
        self.filter.push_str(first_line);
        self.refilter();
    }

    pub fn handle_key(&mut self, key: KeyEvent, composer: &mut Composer) -> InputResult {
        if key.kind == KeyEventKind::Release {
            return InputResult::Unhandled;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Tab, _) => self.try_complete(composer),
            (KeyCode::Enter, m) if !m.contains(KeyModifiers::SHIFT) => {
                self.try_submit_or_complete(composer)
            }
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
        composer.set_text(&self.entries[entry_idx].command);
        self.completed = true;
        InputResult::Handled
    }

    fn try_submit_or_complete(&mut self, composer: &mut Composer) -> InputResult {
        if self.has_exact_match() {
            return InputResult::Unhandled;
        }
        let Some(&entry_idx) = self.matches.get(self.selected) else {
            return InputResult::Unhandled;
        };
        composer.set_text(&self.entries[entry_idx].command);
        self.completed = true;
        InputResult::Unhandled
    }

    fn has_exact_match(&self) -> bool {
        self.matches
            .iter()
            .any(|&entry_idx| self.entries[entry_idx].command == self.filter)
    }

    fn refilter(&mut self) {
        self.matches.clear();
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.command.starts_with(&self.filter) {
                self.matches.push(idx);
            }
        }
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }
}

/// Build the combined slash entry list shared by every popup instance.
/// Built-ins come first, then one entry per skill keyed as `/<skill-name>`
/// with the skill's description shown alongside.
pub fn build_slash_entries(skills: &nav_core::Catalog) -> Arc<[SlashEntry]> {
    let mut entries: Vec<SlashEntry> = BUILTIN_SLASH_COMMANDS
        .iter()
        .map(|cmd| SlashEntry::builtin(cmd))
        .collect();
    for skill in skills.iter() {
        entries.push(SlashEntry {
            command: format!("/{}", skill.name),
            description: Some(skill.description.clone()),
        });
    }
    entries.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nav_core::{Catalog, Skill, SkillScope};

    fn make_catalog() -> Catalog {
        Catalog::new(vec![Skill {
            name: "foo".into(),
            description: "do foo".into(),
            skill_md_path: "/tmp/foo/SKILL.md".into(),
            skill_dir: "/tmp/foo".into(),
            scope: SkillScope::Project,
        }])
    }

    #[test]
    fn builtin_entries_are_present() {
        let entries = build_slash_entries(&Catalog::default());
        let commands: Vec<&str> = entries.iter().map(|e| e.command.as_str()).collect();
        for built in BUILTIN_SLASH_COMMANDS {
            assert!(commands.contains(built), "missing {built}");
        }
    }

    #[test]
    fn catalog_skills_appear_as_slash_entries() {
        let entries = build_slash_entries(&make_catalog());
        let foo = entries
            .iter()
            .find(|e| e.command == "/foo")
            .expect("/foo entry");
        assert_eq!(foo.description.as_deref(), Some("do foo"));
    }

    #[test]
    fn refilter_narrows_by_prefix() {
        let mut popup = SlashCommandPopup::new(build_slash_entries(&make_catalog()));
        popup.on_composer_text_changed("/fo");
        let commands: Vec<&str> = popup.matches().iter().map(|e| e.command.as_str()).collect();
        assert_eq!(commands, vec!["/foo"]);
    }

    #[test]
    fn refilter_finds_builtin() {
        let mut popup = SlashCommandPopup::new(build_slash_entries(&Catalog::default()));
        popup.on_composer_text_changed("/he");
        let commands: Vec<&str> = popup.matches().iter().map(|e| e.command.as_str()).collect();
        assert_eq!(commands, vec!["/help"]);
    }
}
