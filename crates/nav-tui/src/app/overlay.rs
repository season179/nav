use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::ChatWidget;
use crate::bottom_pane::InputResult;

/// Minimal full-screen overlay trait used by app-level overlay UIs.
pub(crate) trait AppOverlay {
    /// Refresh any data this overlay needs for the next render pass.
    fn prepare(&mut self, _chat: &ChatWidget, _width: u16, _animation_tick: u64) {}

    /// Route one key event. `Handled` keeps the event from reaching
    /// composer-level input; `Unhandled` is available for future multi-layer
    /// routing.
    fn handle_key(&mut self, key: KeyEvent) -> InputResult;

    /// Whether this overlay should be dismissed.
    fn is_complete(&self) -> bool;

    /// Render the overlay into the given rect.
    fn render(&self, area: Rect, buf: &mut Buffer);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TranscriptCacheKey {
    terminal_width: u16,
    active_cell_revision: u64,
    stream_continuation_flag: bool,
    animation_tick: u64,
}

/// Full-screen transcript viewer shown from Ctrl+T.
pub(crate) struct TranscriptOverlay {
    dismissed: bool,
    cache_key: Option<TranscriptCacheKey>,
    cached_lines: Vec<Line<'static>>,
    scroll_offset: Cell<usize>,
    content_height: Cell<usize>,
    visible_height: Cell<usize>,
}

impl TranscriptOverlay {
    pub(crate) fn new() -> Self {
        Self {
            dismissed: false,
            cache_key: None,
            cached_lines: Vec::new(),
            scroll_offset: Cell::new(0),
            content_height: Cell::new(0),
            visible_height: Cell::new(0),
        }
    }

    fn max_scroll(&self) -> usize {
        let visible = self.visible_height.get().max(1);
        self.content_height.get().saturating_sub(visible)
    }

    fn clamp_scroll(&self) {
        self.set_scroll_offset(self.scroll_offset.get());
    }

    fn set_scroll_offset(&self, offset: usize) {
        self.scroll_offset.set(offset.min(self.max_scroll()));
    }

    fn page_step(&self) -> usize {
        self.visible_height.get().saturating_sub(1).max(1)
    }
}

impl AppOverlay for TranscriptOverlay {
    fn prepare(&mut self, chat: &ChatWidget, width: u16, animation_tick: u64) {
        let key = TranscriptCacheKey {
            terminal_width: width.max(1),
            active_cell_revision: chat.transcript_revision(),
            stream_continuation_flag: chat.has_live_tail(),
            animation_tick,
        };
        if self.cache_key == Some(key) {
            return;
        }

        let was_unprepared = self.cache_key.is_none();
        let was_at_bottom = self.scroll_offset.get() >= self.max_scroll();
        let mut lines = chat.transcript_lines(key.terminal_width);
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "No transcript yet.",
                Style::default().fg(Color::DarkGray),
            )));
        }
        self.content_height.set(lines.len());
        self.cached_lines = lines;
        self.cache_key = Some(key);
        if was_unprepared || was_at_bottom {
            self.scroll_offset.set(self.max_scroll());
        } else {
            self.clamp_scroll();
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> InputResult {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.dismissed = true,
            KeyCode::Up => {
                self.set_scroll_offset(self.scroll_offset.get().saturating_sub(1));
            }
            KeyCode::Down => {
                self.set_scroll_offset(self.scroll_offset.get().saturating_add(1));
            }
            KeyCode::PageUp => {
                self.set_scroll_offset(self.scroll_offset.get().saturating_sub(self.page_step()));
            }
            KeyCode::PageDown => {
                self.set_scroll_offset(self.scroll_offset.get().saturating_add(self.page_step()));
            }
            KeyCode::Home => self.scroll_offset.set(0),
            KeyCode::End => self.set_scroll_offset(usize::MAX),
            _ => {}
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

        Clear.render(area, buf);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Transcript")
            .title_bottom("Esc/q close  Arrows/Pg/Home/End scroll");
        let inner = block.inner(area);
        block.render(area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        self.visible_height.set(inner.height as usize);
        self.content_height.set(self.cached_lines.len());
        self.clamp_scroll();

        let skip = self.scroll_offset.get();
        let visible = inner.height as usize;
        let lines: Vec<Line<'static>> = self
            .cached_lines
            .iter()
            .skip(skip)
            .take(visible)
            .cloned()
            .collect();
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }
}

/// App overlay slot. New overlays can be added as variants and shared
/// behavior stays in one place.
pub(crate) enum Overlay {
    Transcript(TranscriptOverlay),
}

impl Overlay {
    pub(crate) fn transcript() -> Self {
        Self::Transcript(TranscriptOverlay::new())
    }
}

impl AppOverlay for Overlay {
    fn prepare(&mut self, chat: &ChatWidget, width: u16, animation_tick: u64) {
        match self {
            Self::Transcript(inner) => inner.prepare(chat, width, animation_tick),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> InputResult {
        match self {
            Self::Transcript(inner) => inner.handle_key(key),
        }
    }

    fn is_complete(&self) -> bool {
        match self {
            Self::Transcript(inner) => inner.is_complete(),
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::Transcript(inner) => inner.render(area, buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn transcript_overlay_renders_finalized_and_live_tail() {
        let mut chat = ChatWidget::new();
        chat.push_user("hello from history");
        chat.ingest(nav_core::AgentEvent::AssistantMessageDelta {
            text: "live answer".to_string(),
        });

        let mut overlay = TranscriptOverlay::new();
        overlay.prepare(&chat, 40, 0);

        let rendered = overlay
            .cached_lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("hello from history"), "{rendered}");
        assert!(rendered.contains("live answer"), "{rendered}");
    }

    #[test]
    fn transcript_overlay_scroll_keys_are_clamped() {
        let mut chat = ChatWidget::new();
        for i in 0..20 {
            chat.push_user(format!("line {i}"));
        }

        let mut overlay = TranscriptOverlay::new();
        overlay.prepare(&chat, 30, 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 8));
        overlay.render(buf.area, &mut buf);
        let max = overlay.max_scroll();
        assert_eq!(overlay.scroll_offset.get(), max);

        overlay.handle_key(key(KeyCode::Home));
        assert_eq!(overlay.scroll_offset.get(), 0);
        overlay.handle_key(key(KeyCode::Up));
        assert_eq!(overlay.scroll_offset.get(), 0);
        overlay.handle_key(key(KeyCode::End));
        assert_eq!(overlay.scroll_offset.get(), max);
        overlay.handle_key(key(KeyCode::Down));
        assert_eq!(overlay.scroll_offset.get(), max);
        overlay.handle_key(key(KeyCode::PageUp));
        assert!(overlay.scroll_offset.get() < max);
        overlay.handle_key(key(KeyCode::PageDown));
        assert_eq!(overlay.scroll_offset.get(), max);
    }
}
