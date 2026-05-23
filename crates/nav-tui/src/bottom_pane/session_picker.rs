use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::composer::Composer;
use super::list_picker::{ListPicker, ListPickerItem};
use super::view::{BottomPaneView, InputResult};
use crate::theme::Theme;

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

/// Maximum session rows shown at once.
const MAX_VISIBLE: usize = 8;

impl ListPickerItem for SessionPickerEntry {
    fn render_line(&self, selected: bool, theme: &Theme) -> Line<'static> {
        let bg = Style::default().bg(theme.popup_bg);
        let row_style = if selected {
            bg.fg(ratatui::style::Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            bg.fg(ratatui::style::Color::Gray)
        };
        let name = self.name.as_deref().unwrap_or("(unnamed)").to_string();
        let title = self.title.as_deref().unwrap_or("(no prompt yet)").to_string();
        let turn_word = if self.turn_count == 1 {
            "turn"
        } else {
            "turns"
        };
        Line::from(vec![
            Span::styled("  ", row_style),
            Span::styled(self.id.clone(), row_style),
            Span::styled("  ", row_style),
            Span::styled(name, row_style),
            Span::styled(
                format!(
                    "  created={} active={} {} {turn_word}  ",
                    self.created_at, self.last_active, self.turn_count
                ),
                bg.fg(ratatui::style::Color::DarkGray),
            ),
            Span::styled(title, bg.fg(ratatui::style::Color::Gray)),
        ])
    }
}

/// Bottom-pane popup for choosing a stored session to resume.
///
/// Delegates navigation and rendering to [`ListPicker<SessionPickerEntry>`];
/// this wrapper only exists to expose `take_selection` through the
/// `Any`-downcast path in `key_handling.rs`.
pub struct SessionPickerPopup {
    picker: ListPicker<SessionPickerEntry>,
}

impl SessionPickerPopup {
    pub fn new(entries: Vec<SessionPickerEntry>) -> Self {
        Self::new_with_theme(entries, Theme::default())
    }

    pub fn new_with_theme(entries: Vec<SessionPickerEntry>, theme: Theme) -> Self {
        Self {
            picker: ListPicker::new(entries, MAX_VISIBLE, theme),
        }
    }

    pub fn take_selection(&mut self) -> Option<String> {
        self.picker.take_selection().map(|e| e.id)
    }
}

impl BottomPaneView for SessionPickerPopup {
    fn handle_key(&mut self, key: KeyEvent, _composer: &mut Composer) -> InputResult {
        self.picker.handle_navigation_key(key)
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
}
