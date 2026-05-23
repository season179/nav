//! Generic list-navigation component used by popup overlays.
//!
//! [`ListPicker<T>`] owns a filtered slice of items and provides:
//!
//! - **Navigation** — Up / Down / j / k / Esc / Enter
//! - **Selection state** — `completed` flag, `take_selection()` for consuming
//!   the picked item
//! - **Rendering** — theme-aware list with cyan highlight for the selected row
//!   and dim styling for the rest, capped to a configurable `max_visible`
//!
//! Each item type implements [`ListPickerItem`] so the picker doesn't know
//! about slash commands vs. session rows — it just renders whatever the item
//! tells it to. Parent popups own a `ListPicker` and delegate
//! `BottomPaneView` methods to it, keeping the popup itself a thin wrapper
//! that only adds domain-specific behaviour (filtering, Tab completion, etc.).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Widget};

use super::view::InputResult;
use crate::theme::Theme;

/// Render a single row inside a [`ListPicker`].
///
/// `selected` is `true` for the highlighted row; the implementer should use
/// the cyan/bold style vs. the dim grey style accordingly. `theme` is passed
/// so each row can pull the popup background colour.
///
/// Implementations must return [`Line<'static>`], so every [`Span`] must
/// contain **owned** strings (`to_string()`, not `as_str()`).
pub trait ListPickerItem: Clone {
    fn render_line(&self, selected: bool, theme: &Theme) -> Line<'static>;
}

/// Reusable list-navigation component.
///
/// Not a [`super::view::BottomPaneView`] itself — parent popups implement the
/// trait and delegate to this struct for the common navigation, rendering, and
/// state management. This keeps `ListPicker` agnostic about what the items
/// *mean* (slash commands, sessions, files, …) while still collapsing the
/// duplicated Up / Down / Esc / Enter boilerplate into one place.
pub struct ListPicker<T: ListPickerItem> {
    items: Vec<T>,
    theme: Theme,
    selected: usize,
    completed: bool,
    max_visible: usize,
    /// `Some(idx)` after the user confirms via Enter; `None` after Esc.
    /// Consumed by [`Self::take_selection`].
    selection: Option<usize>,
}

impl<T: ListPickerItem> ListPicker<T> {
    pub fn new(items: Vec<T>, max_visible: usize, theme: Theme) -> Self {
        Self {
            items,
            theme,
            selected: 0,
            completed: false,
            max_visible,
            selection: None,
        }
    }

    // ── Navigation ────────────────────────────────────────────────────

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        let cap = self.visible_count();
        if self.selected + 1 < cap {
            self.selected += 1;
        }
    }

    /// Mark the currently highlighted row as the selection and set
    /// `completed`. No-op when the item list is empty.
    pub fn confirm(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selection = Some(self.selected);
        self.completed = true;
    }

    /// Dismiss without picking anything.
    pub fn cancel(&mut self) {
        self.selection = None;
        self.completed = true;
    }

    // ── State queries ─────────────────────────────────────────────────

    pub fn is_complete(&self) -> bool {
        self.completed
    }

    /// Consume the selected item, if any. Returns `None` after Esc or if the
    /// list was empty.
    pub fn take_selection(&mut self) -> Option<T> {
        let idx = self.selection.take()?;
        Some(self.items.remove(idx))
    }

    /// Borrow the currently highlighted item without consuming it.
    pub fn selected_item(&self) -> Option<&T> {
        self.items.get(self.selected)
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    // ── Item management ───────────────────────────────────────────────

    /// Replace the visible items (e.g. after re-filtering). Resets
    /// `selected` to 0 and clears any prior selection so the user lands
    /// on the top-ranked result.
    pub fn set_items(&mut self, items: Vec<T>) {
        self.items = items;
        self.selected = 0;
        self.selection = None;
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of rows that will actually be rendered (capped by
    /// `max_visible`).
    fn visible_count(&self) -> usize {
        self.items.len().min(self.max_visible)
    }

    // ── Rendering ─────────────────────────────────────────────────────

    pub fn desired_height(&self, _width: u16) -> u16 {
        self.visible_count().max(1) as u16
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let bg = Style::default().bg(self.theme.popup_bg);
        Block::default().style(bg).render(area, buf);

        if self.items.is_empty() {
            let hint = ratatui::text::Span::styled(
                "  no items",
                bg.fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            );
            Paragraph::new(Line::from(hint))
                .style(bg)
                .render(area, buf);
            return;
        }

        let lines: Vec<Line<'_>> = self
            .items
            .iter()
            .take(self.max_visible)
            .enumerate()
            .map(|(row, item)| item.render_line(row == self.selected, &self.theme))
            .collect();
        Paragraph::new(lines).style(bg).render(area, buf);
    }

    // ── Key handling ──────────────────────────────────────────────────

    /// Handle standard list-navigation keys:
    ///
    /// - **Up** / **k** — move selection up
    /// - **Down** / **j** — move selection down
    /// - **Enter** — confirm selection
    /// - **Esc** — cancel
    ///
    /// Returns [`InputResult::Handled`] when the key is recognised,
    /// [`InputResult::Unhandled`] otherwise. Parent popups should intercept
    /// domain-specific keys (Tab, special Enter, etc.) *before* calling this
    /// so they take priority.
    pub fn handle_navigation_key(&mut self, key: KeyEvent) -> InputResult {
        if key.kind == KeyEventKind::Release {
            return InputResult::Unhandled;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                self.move_up();
                InputResult::Handled
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                self.move_down();
                InputResult::Handled
            }
            (KeyCode::Enter, m) if !m.contains(KeyModifiers::SHIFT) => {
                self.confirm();
                InputResult::Handled
            }
            (KeyCode::Esc, _) => {
                self.cancel();
                InputResult::Handled
            }
            _ => InputResult::Unhandled,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::text::Span;

    use super::*;

    /// Minimal item type for tests — just renders the label.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestItem {
        label: String,
    }

    impl ListPickerItem for TestItem {
        fn render_line(&self, selected: bool, theme: &Theme) -> Line<'static> {
            let bg = Style::default().bg(theme.popup_bg);
            let style = if selected {
                bg.fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                bg.fg(Color::Gray)
            };
            Line::from(vec![
                Span::styled("  ", style),
                Span::styled(self.label.clone(), style),
            ])
        }
    }

    fn picker(items: &[&str]) -> ListPicker<TestItem> {
        ListPicker::new(
            items
                .iter()
                .map(|s| TestItem {
                    label: s.to_string(),
                })
                .collect(),
            8,
            Theme::default(),
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn new_picker_selects_first_item() {
        let p = picker(&["a", "b", "c"]);
        assert_eq!(p.selected_index(), 0);
        assert_eq!(p.selected_item().unwrap().label, "a");
    }

    #[test]
    fn down_moves_selection() {
        let mut p = picker(&["a", "b", "c"]);
        p.move_down();
        assert_eq!(p.selected_index(), 1);
        p.move_down();
        assert_eq!(p.selected_index(), 2);
        // Clamped — can't go past the last item.
        p.move_down();
        assert_eq!(p.selected_index(), 2);
    }

    #[test]
    fn up_clamps_at_zero() {
        let mut p = picker(&["a", "b", "c"]);
        p.move_up();
        assert_eq!(p.selected_index(), 0);
    }

    #[test]
    fn j_k_navigation() {
        let mut p = picker(&["a", "b", "c"]);
        assert_eq!(
            p.handle_navigation_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            InputResult::Handled
        );
        assert_eq!(p.selected_index(), 1);
        assert_eq!(
            p.handle_navigation_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            InputResult::Handled
        );
        assert_eq!(p.selected_index(), 0);
    }

    #[test]
    fn enter_confirms_selection() {
        let mut p = picker(&["a", "b", "c"]);
        p.move_down();
        assert_eq!(
            p.handle_navigation_key(key(KeyCode::Enter)),
            InputResult::Handled
        );
        assert!(p.is_complete());
        let sel = p.take_selection().unwrap();
        assert_eq!(sel.label, "b");
    }

    #[test]
    fn esc_cancels_without_selection() {
        let mut p = picker(&["a", "b"]);
        assert_eq!(p.handle_navigation_key(key(KeyCode::Esc)), InputResult::Handled);
        assert!(p.is_complete());
        assert!(p.take_selection().is_none());
    }

    #[test]
    fn confirm_is_noop_on_empty_list() {
        let mut p: ListPicker<TestItem> = ListPicker::new(vec![], 8, Theme::default());
        p.confirm();
        // Empty list → completed stays false (nothing to select).
        assert!(!p.is_complete());
    }

    #[test]
    fn set_items_resets_selection() {
        let mut p = picker(&["a", "b", "c"]);
        p.move_down();
        assert_eq!(p.selected_index(), 1);
        p.set_items(vec![TestItem {
            label: "x".into(),
        }]);
        assert_eq!(p.selected_index(), 0);
        assert_eq!(p.items().len(), 1);
    }

    #[test]
    fn desired_height_capped_by_max_visible() {
        let mut p = picker(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        p.max_visible = 5;
        assert_eq!(p.desired_height(80), 5);
    }

    #[test]
    fn desired_height_is_one_when_empty() {
        let p: ListPicker<TestItem> = ListPicker::new(vec![], 8, Theme::default());
        assert_eq!(p.desired_height(80), 1);
    }

    #[test]
    fn unrecognised_key_is_unhandled() {
        let mut p = picker(&["a"]);
        assert_eq!(
            p.handle_navigation_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            InputResult::Unhandled
        );
        assert!(!p.is_complete());
    }

    #[test]
    fn take_selection_consumes_result() {
        let mut p = picker(&["a"]);
        p.confirm();
        assert!(p.take_selection().is_some());
        assert!(p.take_selection().is_none());
    }
}
