//! Inline viewport layout for the main TUI loop.
//!
//! Finalized chat history lives in the terminal's native scrollback (see
//! `crate::insert_history`), not inside ratatui. This module only paints
//! the inline viewport: any in-flight streaming text and the bottom pane.
//! The bottom pane owns its own status bar, overlays, pending-input queue,
//! and composer (see `crate::bottom_pane`); this module just sizes the
//! viewport per-frame and splits it into a streaming chunk + a pane chunk.
//!
//! Viewport boundary math lives next door in [`super::inline_region`] so
//! the edge cases (overflow, shrink-blanking, small screens) can be
//! unit-tested without a terminal backend. `draw_tui` only performs the
//! side-effects (scroll into scrollback, erase vacated rows, resize) the
//! computed [`InlineRegion`] describes.

use anyhow::Result;
use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::Paragraph;
use std::io::Stdout;

use super::inline_region::{InlineRegion, streaming_cap};
use crate::ChatWidget;
use crate::bottom_pane;
use crate::custom_terminal::Terminal;

pub(super) fn draw_tui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    chat: &ChatWidget,
    pane: &bottom_pane::BottomPane,
    screen_w: u16,
    screen_h: u16,
) -> Result<()> {
    // Materialize streaming lines first — the cap depends on the screen
    // budget, and `inline_lines_capped` already prioritizes tool-call
    // placeholders over streaming-assistant tokens so a long reply can't
    // hide an `Exploring`/`Running` row past the cap.
    let cap = streaming_cap(screen_h);
    let streaming_lines = chat.inline_lines_capped(screen_w, cap);
    let streaming_rows = streaming_lines.len() as u16;

    let region = InlineRegion::compute(
        screen_w,
        screen_h,
        streaming_rows,
        pane.desired_height(screen_w),
        terminal.viewport_area,
    );

    // Side-effects must run *before* `set_viewport_area`:
    //   1. Scroll the rows above the old viewport into native scrollback
    //      when the new viewport would overflow the screen floor — this
    //      saves the user-prompt row from being overpainted by the inline
    //      frame.
    //   2. Erase rows the new viewport vacated at the bottom (e.g.
    //      streaming finalize shrinks the inline frame) so stale pixels
    //      below the new composer don't look like duplicated text.
    if let Some(scroll) = region.scroll_above {
        crate::insert_history::scroll_region_above_into_scrollback(
            terminal, scroll.top, scroll.by,
        )?;
    }
    if let Some(rows) = region.blank_rows.clone() {
        let backend = terminal.backend_mut();
        for row in rows {
            queue!(backend, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        }
    }

    terminal.set_viewport_area(region.viewport_area);

    terminal.draw(|f| {
        let area = f.area();
        let chunks = Layout::vertical([
            Constraint::Length(region.streaming_h),
            Constraint::Length(region.pane_h),
        ])
        .split(area);

        if region.streaming_h > 0 {
            f.render_widget(Paragraph::new(streaming_lines), chunks[0]);
        }
        f.render_widget(pane, chunks[1]);
        if let Some((cx, cy)) = pane.cursor_position(chunks[1]) {
            f.set_cursor_position((cx, cy));
        }
    })?;

    Ok(())
}
