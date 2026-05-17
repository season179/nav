use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::composer::Composer;
use super::view::InputResult;

pub const SLASH_COMMANDS: &[&str] = &["/help", "/clear", "/quit", "/resume", "/sessions"];

/// Overlay that filters [`SLASH_COMMANDS`] by composer-buffer prefix and lets
/// the user pick one with Tab / Enter.
pub struct SlashCommandPopup {
    filter: String,
    matches: Vec<&'static str>,
    selected: usize,
    completed: bool,
}

impl SlashCommandPopup {
    pub fn new() -> Self {
        Self {
            filter: String::from("/"),
            matches: SLASH_COMMANDS.to_vec(),
            selected: 0,
            completed: false,
        }
    }

    pub fn matches(&self) -> &[&'static str] {
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
        let lines: Vec<Line<'static>> = self
            .matches
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let style = if i == self.selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(*cmd, style))
            })
            .collect();
        Paragraph::new(lines).render(area, buf);
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
        let Some(cmd) = self.matches.get(self.selected) else {
            return InputResult::Unhandled;
        };
        composer.set_text(cmd);
        self.completed = true;
        InputResult::Handled
    }

    fn refilter(&mut self) {
        self.matches = SLASH_COMMANDS
            .iter()
            .copied()
            .filter(|cmd| cmd.starts_with(&self.filter))
            .collect();
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }
}

impl Default for SlashCommandPopup {
    fn default() -> Self {
        Self::new()
    }
}
