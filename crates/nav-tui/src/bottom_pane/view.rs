use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::approval::ApprovalOverlay;
use super::composer::Composer;
use super::mention_popup::FileMentionPopup;
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
    FileMention(FileMentionPopup),
    Approval(ApprovalOverlay),
}

impl BottomPaneView {
    pub fn handle_key(&mut self, key: KeyEvent, composer: &mut Composer) -> InputResult {
        match self {
            Self::SlashCommand(p) => p.handle_key(key, composer),
            Self::FileMention(p) => p.handle_key(key, composer),
            Self::Approval(p) => p.handle_key(key, composer),
        }
    }

    pub fn is_complete(&self) -> bool {
        match self {
            Self::SlashCommand(p) => p.is_complete(),
            Self::FileMention(p) => p.is_complete(),
            Self::Approval(p) => p.is_complete(),
        }
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        match self {
            Self::SlashCommand(p) => p.desired_height(width),
            Self::FileMention(p) => p.desired_height(width),
            Self::Approval(p) => p.desired_height(width),
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::SlashCommand(p) => p.render(area, buf),
            Self::FileMention(p) => p.render(area, buf),
            Self::Approval(p) => p.render(area, buf),
        }
    }

    pub fn on_composer_text_changed(&mut self, first_line: &str) {
        match self {
            Self::SlashCommand(p) => p.on_composer_text_changed(first_line),
            // The mention popup is updated by BottomPane::reconcile_mention_popup
            // which has access to the composer's cursor — not just the first
            // line — so it doesn't need this generic hook.
            Self::FileMention(_) => {}
            // Approval overlays don't react to composer changes; the modal
            // is decided by keystrokes only.
            Self::Approval(_) => {}
        }
    }
}
