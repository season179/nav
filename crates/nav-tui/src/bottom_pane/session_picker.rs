use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::composer::Composer;
use super::view::InputResult;

/// One row in the recent-session picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPickerEntry {
    pub id: String,
    pub name: Option<String>,
    pub created_at: i64,
    pub last_active: i64,
    pub turn_count: u64,
    pub title: Option<String>,
}

impl SessionPickerEntry {
    pub fn from_summary(summary: &nav_core::SessionSummary) -> Self {
        Self {
            id: summary.id.clone(),
            name: summary.name.clone(),
            created_at: summary.created_at,
            last_active: summary.last_active,
            turn_count: summary.turn_count,
            title: summary.first_user_prompt.clone(),
        }
    }
}

/// Bottom-pane popup for choosing a stored session to resume.
pub struct SessionPickerPopup {
    entries: Vec<SessionPickerEntry>,
    selected: usize,
    completed: bool,
    selected_id: Option<String>,
}

const MAX_VISIBLE: usize = 8;

impl SessionPickerPopup {
    pub fn new(entries: Vec<SessionPickerEntry>) -> Self {
        Self {
            entries,
            selected: 0,
            completed: false,
            selected_id: None,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.completed
    }

    pub fn take_selection(&mut self) -> Option<String> {
        self.selected_id.take()
    }

    pub fn desired_height(&self, _width: u16) -> u16 {
        if self.entries.is_empty() {
            1
        } else {
            self.entries.len().min(MAX_VISIBLE) as u16
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, _composer: &mut Composer) -> InputResult {
        if key.kind == KeyEventKind::Release {
            return InputResult::Unhandled;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Enter, m) if !m.contains(KeyModifiers::SHIFT) => {
                if let Some(entry) = self.entries.get(self.selected) {
                    self.selected_id = Some(entry.id.clone());
                    self.completed = true;
                    InputResult::Handled
                } else {
                    InputResult::Unhandled
                }
            }
            (KeyCode::Up, _) => {
                self.selected = self.selected.saturating_sub(1);
                InputResult::Handled
            }
            (KeyCode::Down, _) => {
                if self.selected + 1 < self.entries.len().min(MAX_VISIBLE) {
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

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let bg = Style::default().bg(crate::theme::POPUP_BG);
        Block::default().style(bg).render(area, buf);

        if self.entries.is_empty() {
            let hint = Span::styled(
                "  no stored sessions",
                bg.fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            );
            Paragraph::new(Line::from(hint)).style(bg).render(area, buf);
            return;
        }

        let lines: Vec<Line<'_>> = self
            .entries
            .iter()
            .take(MAX_VISIBLE)
            .enumerate()
            .map(|(row, entry)| {
                let row_style = if row == self.selected {
                    bg.fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    bg.fg(Color::Gray)
                };
                let name = entry.name.as_deref().unwrap_or("(unnamed)");
                let title = entry.title.as_deref().unwrap_or("(no prompt yet)");
                let turn_word = if entry.turn_count == 1 {
                    "turn"
                } else {
                    "turns"
                };
                Line::from(vec![
                    Span::styled("  ", row_style),
                    Span::styled(entry.id.as_str(), row_style),
                    Span::styled("  ", row_style),
                    Span::styled(name.to_string(), row_style),
                    Span::styled(
                        format!(
                            "  created={} active={} {} {turn_word}  ",
                            entry.created_at, entry.last_active, entry.turn_count
                        ),
                        bg.fg(Color::DarkGray),
                    ),
                    Span::styled(title.to_string(), bg.fg(Color::Gray)),
                ])
            })
            .collect();
        Paragraph::new(lines).style(bg).render(area, buf);
    }
}
