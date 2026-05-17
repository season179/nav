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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use crate::theme::COMPOSER_BG;

mod composer;
mod slash_popup;
mod view;

pub use composer::{Composer, ComposerEvent};
pub use slash_popup::{SLASH_COMMANDS, SlashCommandPopup};
pub use view::{BottomPaneView, InputResult};

pub struct BottomPane {
    composer: Composer,
    view: Option<BottomPaneView>,
    /// Set when the user dismisses the slash popup (Esc). Suppresses
    /// auto-reopen on the same `/…` text so the user can press Enter to
    /// submit the slash command as a plain prompt. Cleared once the
    /// composer no longer starts with `/`.
    slash_popup_suppressed: bool,
}

impl BottomPane {
    pub fn new() -> Self {
        Self {
            composer: Composer::new(),
            view: None,
            slash_popup_suppressed: false,
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
                        self.slash_popup_suppressed = true;
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
        // Composer always reserves at least 3 rows so the filled background
        // reads as a distinct input block (one row of `›` + text plus a row
        // of padding above and below — matches the codex visual weight).
        let composer_h = self.composer.desired_height(width).max(3);
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

        if !slash_active {
            self.slash_popup_suppressed = false;
        }

        match (&mut self.view, slash_active) {
            (None, true) if !self.slash_popup_suppressed => {
                let mut popup = SlashCommandPopup::new();
                popup.on_composer_text_changed(first);
                if !popup.is_complete() {
                    self.view = Some(BottomPaneView::SlashCommand(popup));
                }
            }
            (None, _) => {}
            (Some(view), true) => {
                view.on_composer_text_changed(first);
                if view.is_complete() {
                    self.view = None;
                }
            }
            (Some(BottomPaneView::SlashCommand(_)), false) => {
                self.view = None;
            }
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
        let [overlay_rect, composer_outer] =
            Layout::vertical([Constraint::Length(overlay_h), Constraint::Min(1)]).areas(area);

        if let Some(view) = self.view.as_ref()
            && overlay_rect.height > 0
        {
            view.render(overlay_rect, buf);
        }

        if composer_outer.height > 0 {
            // Fill the entire composer block with the input background so the
            // gutter, padding rows and text all sit on the same coloured rect.
            let bg = Style::default().bg(COMPOSER_BG);
            Block::default().style(bg).render(composer_outer, buf);

            // One row of top padding so the prompt + text sit visually centred
            // inside the block when only one line is composed.
            let text_top = composer_outer.y.saturating_add(1);
            let text_rect = Rect {
                x: composer_outer.x,
                y: text_top,
                width: composer_outer.width,
                height: composer_outer.height.saturating_sub(1),
            };

            let [gutter, content] =
                Layout::horizontal([Constraint::Length(2), Constraint::Min(0)]).areas(text_rect);

            let prompt_style = if self.composer.is_empty() {
                bg.fg(Color::DarkGray)
            } else {
                bg.fg(Color::White).add_modifier(Modifier::BOLD)
            };
            let prompt = Paragraph::new(Line::from(Span::styled("›", prompt_style))).style(bg);
            let gutter_first = Rect {
                height: 1.min(gutter.height),
                ..gutter
            };
            prompt.render(gutter_first, buf);
            self.composer.render(content, buf);
        }
    }
}
