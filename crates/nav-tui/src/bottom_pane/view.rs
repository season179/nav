//! Trait for overlays that float above the composer inside the bottom pane.
//!
//! The bottom pane hosts at most one overlay at a time (popup, modal, picker).
//! Each overlay implements [`BottomPaneView`]; the pane drives them through
//! dynamic dispatch via `Option<Box<dyn BottomPaneView>>`, so adding a new
//! popup means writing one `impl BottomPaneView for NewPopup` in its own file.
//!
//! A new **modal** popup (one whose lifecycle is decision-driven, not text-
//! driven) needs no changes to `key_handling.rs`: the default
//! [`BottomPaneView::is_text_driven`] is `false`, which keeps the overlay on
//! screen while the user types. Text-driven popups (slash, mention) override
//! that method and own the matching downcast paths in
//! `reconcile_popups`. Popups with side effects on completion (decisions,
//! selections, suppression flags) still need `Any`-downcasts in
//! `key_handling.rs::handle_key` to surface those effects to the pane;
//! that's unavoidable until we route results through an event bus.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::composer::Composer;

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

/// Blanket helper so each popup gets `as_any` / `as_any_mut` for free.
/// Required because [`BottomPaneView`] is used through `Box<dyn _>` but the
/// pane occasionally needs to recover the concrete type (e.g. to call
/// `take_decision` on an `ApprovalOverlay`).
pub trait AsAny {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: 'static> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Behavior contract for any overlay the bottom pane can host on top of the
/// composer.
///
/// **Lifetime bound:** the [`AsAny`] supertrait requires `'static`, so
/// implementing types cannot hold borrowed fields. Clone or own data instead.
/// Forgetting this surfaces as "trait `AsAny` is not implemented" rather than
/// a clear lifetime diagnostic.
pub trait BottomPaneView: AsAny {
    /// Route a key. Overlays see keys first; return [`InputResult::Unhandled`]
    /// to forward to the composer.
    fn handle_key(&mut self, key: KeyEvent, composer: &mut Composer) -> InputResult;

    /// True once the overlay has finished its job (decision made, picker
    /// selected, Esc'd, etc.). The pane drops the overlay after each
    /// `Handled` key when this returns true.
    fn is_complete(&self) -> bool;

    /// Rows the overlay would like to occupy. Caller decides the actual
    /// height available.
    fn desired_height(&self, width: u16) -> u16;

    /// Paint the overlay into the given rect.
    fn render(&self, area: Rect, buf: &mut Buffer);

    /// Hook fired when the composer's first line of text changes. Used by the
    /// slash popup to refresh its filter. Default no-op so passive popups
    /// (approval, session picker, mention popup) don't have to opt out.
    fn on_composer_text_changed(&mut self, _first_line: &str) {}

    /// True if this overlay's lifecycle is *driven by composer text* — i.e.
    /// the overlay opens, refreshes, and closes in response to what the user
    /// is typing (slash popup on `/…`, mention popup on `@token`). False for
    /// modal overlays whose lifecycle is decision-driven (approval, session
    /// picker, future confirmation dialogs, list pickers).
    ///
    /// `reconcile_popups` uses this to decide whether composer-text events
    /// can disturb the active overlay. **Default `false`** so passive popups
    /// don't get nuked by the slow path the first time the user types a
    /// character that doesn't match slash or mention rules.
    fn is_text_driven(&self) -> bool {
        false
    }
}
