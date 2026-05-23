use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::bottom_pane::InputResult;

/// Minimal full-screen overlay trait used by app-level overlay UIs.
pub(crate) trait AppOverlay {
    /// Route one key event. `Handled` keeps the event from reaching
    /// composer-level input; `Unhandled` is available for future multi-layer
    /// routing.
    fn handle_key(&mut self, key: KeyEvent) -> InputResult;

    /// Whether this overlay should be dismissed.
    fn is_complete(&self) -> bool;

    /// Render the overlay into the given rect.
    fn render(&self, area: Rect, buf: &mut Buffer);
}

/// Test overlay for OVR-00 plumbing and round-trip validation.
pub(crate) struct TestOverlay {
    dismissed: bool,
}

impl TestOverlay {
    pub(crate) fn new() -> Self {
        Self { dismissed: false }
    }
}

impl AppOverlay for TestOverlay {
    fn handle_key(&mut self, key: KeyEvent) -> InputResult {
        if matches!(key.code, KeyCode::Esc) {
            self.dismissed = true;
        }
        InputResult::Handled
    }

    fn is_complete(&self) -> bool {
        self.dismissed
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let lines = vec![
            Line::from(Span::styled(
                "Test Overlay",
                Style::default().fg(Color::Cyan),
            )),
            Line::from(""),
            Line::from("Alt-screen overlay plumbing test surface."),
            Line::from(""),
            Line::from("Press Esc to close."),
            Line::from(""),
            Line::from("Any key is swallowed by the overlay."),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Overlay"))
            .render(area, buf);
    }
}

/// App overlay slot. New overlays can be added as variants and shared
/// behavior stays in one place.
pub(crate) enum Overlay {
    Test(TestOverlay),
}

impl Overlay {
    pub(crate) fn test() -> Self {
        Self::Test(TestOverlay::new())
    }
}

impl AppOverlay for Overlay {
    fn handle_key(&mut self, key: KeyEvent) -> InputResult {
        match self {
            Self::Test(inner) => inner.handle_key(key),
        }
    }

    fn is_complete(&self) -> bool {
        match self {
            Self::Test(inner) => inner.is_complete(),
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::Test(inner) => inner.render(area, buf),
        }
    }
}
