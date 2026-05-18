use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::composer::Composer;
use super::view::InputResult;

/// Built-in slash commands the TUI always offers.
pub const BUILTIN_SLASH_COMMANDS: &[&str] = &["/help", "/clear", "/quit", "/resume", "/sessions"];

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
/// lets the user pick one with Tab / Enter. The entry list combines built-in
/// commands with the discovered skills catalog (one entry per skill).
pub struct SlashCommandPopup {
    entries: Vec<SlashEntry>,
    filter: String,
    matches: Vec<SlashEntry>,
    selected: usize,
    completed: bool,
}

impl SlashCommandPopup {
    pub fn new(entries: Vec<SlashEntry>) -> Self {
        let matches = entries.clone();
        Self {
            entries,
            filter: String::from("/"),
            matches,
            selected: 0,
            completed: false,
        }
    }

    pub fn matches(&self) -> &[SlashEntry] {
        &self.matches
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
        let lines: Vec<Line<'static>> = self
            .matches
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let row_style = if i == self.selected {
                    bg.fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    bg.fg(Color::Gray)
                };
                let mut spans = vec![
                    Span::styled("  ", row_style),
                    Span::styled(entry.command.clone(), row_style),
                ];
                if let Some(desc) = entry.description.as_ref() {
                    spans.push(Span::styled("  ", row_style));
                    spans.push(Span::styled(desc.clone(), bg.fg(Color::DarkGray)));
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
        let Some(entry) = self.matches.get(self.selected) else {
            return InputResult::Unhandled;
        };
        composer.set_text(&entry.command);
        self.completed = true;
        InputResult::Handled
    }

    fn refilter(&mut self) {
        self.matches = self
            .entries
            .iter()
            .filter(|entry| entry.command.starts_with(&self.filter))
            .cloned()
            .collect();
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }
}

/// Build the combined entry list used by the slash popup.
///
/// Built-in commands come first, followed by one entry per skill keyed as
/// `/<skill-name>` with the skill's description shown to the right of the
/// command name.
pub fn build_slash_entries(skills: &nav_core::Catalog) -> Vec<SlashEntry> {
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
    entries
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
