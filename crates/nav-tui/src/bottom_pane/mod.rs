//! Bottom-pane composer and overlay stack.
//!
//! The bottom pane is the input region at the bottom of the TUI. It owns a
//! [`Composer`] for free-form text and an optional [`BottomPaneView`] overlay
//! that floats above the composer. Input is routed view-first: any active
//! overlay sees the key first, and the composer only sees keys the overlay
//! explicitly returns [`InputResult::Unhandled`] for.

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Widget;

mod composer;
mod slash_popup;
mod view;

pub use composer::{Composer, ComposerEvent};
pub use slash_popup::{SLASH_COMMANDS, SlashCommandPopup};
pub use view::{BottomPaneView, InputResult};

pub struct BottomPane {
    composer: Composer,
    view: Option<BottomPaneView>,
}

impl BottomPane {
    pub fn new() -> Self {
        Self {
            composer: Composer::new(),
            view: None,
        }
    }

    pub fn composer(&self) -> &Composer {
        &self.composer
    }

    pub fn has_overlay(&self) -> bool {
        self.view.is_some()
    }

    /// Returns the slash-command popup if it is the active overlay.
    pub fn slash_popup(&self) -> Option<&SlashCommandPopup> {
        match &self.view {
            Some(BottomPaneView::SlashCommand(p)) => Some(p),
            _ => None,
        }
    }

    /// Route a keystroke. Overlays see the key first; the composer only sees
    /// it when the overlay returns [`InputResult::Unhandled`].
    pub fn handle_key(&mut self, key: KeyEvent) -> ComposerEvent {
        if let Some(view) = self.view.as_mut() {
            match view.handle_key(key, &mut self.composer) {
                InputResult::Handled => {
                    if view.is_complete() {
                        self.view = None;
                    }
                    self.reconcile_slash_popup();
                    return ComposerEvent::Nothing;
                }
                InputResult::Unhandled => {}
            }
        }
        let event = self.composer.handle_key(key);
        self.reconcile_slash_popup();
        event
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        let composer_h = self.composer.desired_height(width);
        let overlay_h = self
            .view
            .as_ref()
            .map(|v| v.desired_height(width))
            .unwrap_or(0);
        composer_h.saturating_add(overlay_h)
    }

    fn reconcile_slash_popup(&mut self) {
        let single_line = self.composer.line_count() == 1;
        let first = self.composer.first_line();
        let slash_active = single_line && first.starts_with('/');

        match (&mut self.view, slash_active) {
            (None, true) => {
                let mut popup = SlashCommandPopup::new();
                popup.on_composer_text_changed(first);
                if !popup.is_complete() {
                    self.view = Some(BottomPaneView::SlashCommand(popup));
                }
            }
            (Some(view), true) => {
                view.on_composer_text_changed(first);
                if view.is_complete() {
                    self.view = None;
                }
            }
            (Some(BottomPaneView::SlashCommand(_)), false) => {
                self.view = None;
            }
            (None, false) => {}
        }
    }
}

impl Default for BottomPane {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for &BottomPane {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let overlay_h = self
            .view
            .as_ref()
            .map(|v| v.desired_height(area.width))
            .unwrap_or(0);
        let [overlay_rect, composer_rect] =
            Layout::vertical([Constraint::Length(overlay_h), Constraint::Min(0)]).areas(area);
        if let Some(view) = self.view.as_ref()
            && overlay_rect.height > 0
        {
            view.render(overlay_rect, buf);
        }
        if composer_rect.height > 0 {
            self.composer.render(composer_rect, buf);
        }
    }
}
