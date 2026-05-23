use crossterm::event::KeyEvent;
use nav_core::cli::ModelLine;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::composer::Composer;
use super::list_picker::{ListPicker, ListPickerItem};
use super::view::{BottomPaneView, InputResult};
use crate::theme::Theme;

/// One row in the model picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPickerEntry {
    pub selector: String,
    pub provider_display_name: String,
}

impl ModelPickerEntry {
    pub fn from_line(line: &ModelLine) -> Self {
        Self {
            selector: line.selector.clone(),
            provider_display_name: line.provider_display_name.clone(),
        }
    }
}

const MAX_VISIBLE: usize = 8;

impl ListPickerItem for ModelPickerEntry {
    fn render_line(&self, selected: bool, theme: &Theme) -> Line<'static> {
        let bg = Style::default().bg(theme.popup_bg);
        let row_style = if selected {
            bg.fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            bg.fg(Color::Gray)
        };
        Line::from(vec![
            Span::styled("  ", row_style),
            Span::styled(self.selector.clone(), row_style),
            Span::styled(
                format!("  ({})", self.provider_display_name),
                bg.fg(Color::DarkGray),
            ),
        ])
    }
}

/// Bottom-pane popup for choosing a configured model.
pub struct ModelPickerPopup {
    picker: ListPicker<ModelPickerEntry>,
}

impl ModelPickerPopup {
    pub fn new_with_theme(
        entries: Vec<ModelPickerEntry>,
        current_model: Option<&str>,
        theme: Theme,
    ) -> Self {
        let mut picker = ListPicker::new(entries, MAX_VISIBLE, theme);
        if let Some(current) = current_model
            && let Some(idx) = picker
                .items()
                .iter()
                .position(|entry| entry.selector == current)
        {
            picker.set_selected_index(idx);
        }
        Self { picker }
    }

    pub fn take_selection(&mut self) -> Option<String> {
        self.picker.take_selection().map(|entry| entry.selector)
    }
}

impl BottomPaneView for ModelPickerPopup {
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
