use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::composer::Composer;
use super::slash_popup::SlashCommandPopup;

/// Outcome of an overlay's attempt to handle a key.
///
/// Overlays always see the key first. Returning [`InputResult::Handled`] stops
/// dispatch; returning [`InputResult::Unhandled`] forwards the key on to the
/// underlying [`Composer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputResult {
    Handled,
    Unhandled,
}

/// Overlays the bottom pane can host on top of the composer.
pub enum BottomPaneView {
    SlashCommand(SlashCommandPopup),
}

impl BottomPaneView {
    pub fn handle_key(&mut self, key: KeyEvent, composer: &mut Composer) -> InputResult {
        match self {
            Self::SlashCommand(p) => p.handle_key(key, composer),
        }
    }

    pub fn is_complete(&self) -> bool {
        match self {
            Self::SlashCommand(p) => p.is_complete(),
        }
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        match self {
            Self::SlashCommand(p) => p.desired_height(width),
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::SlashCommand(p) => p.render(area, buf),
        }
    }

    pub fn on_composer_text_changed(&mut self, first_line: &str) {
        match self {
            Self::SlashCommand(p) => p.on_composer_text_changed(first_line),
        }
    }
}
