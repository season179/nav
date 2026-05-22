//! Inline viewport layout for the main TUI loop.
//!
//! Finalized chat history lives in the terminal's native scrollback (see
//! `crate::insert_history`), not inside ratatui. This module only paints
//! the inline viewport: any in-flight streaming text, the composer, and
//! the status bar. The viewport is sized per-frame and anchored to the
//! bottom of the screen.

use anyhow::Result;
use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::terminal::{Clear, ClearType};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Paragraph;
use std::io::Stdout;

use crate::ChatWidget;
use crate::bottom_pane;
use crate::custom_terminal::Terminal;

use super::status_bar::{AgentState, StatusBar};

/// Cap the streaming preview at this many rows so a long in-flight reply
/// can't shove the composer off-screen. Once the reply finalizes it goes
/// to scrollback and the cap stops mattering.
const MAX_STREAMING_ROWS: u16 = 16;

/// Bottom-anchored status bar height.
const STATUS_ROWS: u16 = 1;

pub(super) struct TuiStatus<'a> {
    pub model: &'a str,
    pub cwd_short: &'a str,
    pub branch: Option<&'a str>,
    pub dirty: bool,
    pub state: AgentState,
    pub tokens_input: u64,
    pub context_window: u64,
}

pub(super) fn draw_tui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    chat: &ChatWidget,
    pane: &bottom_pane::BottomPane,
    status: TuiStatus<'_>,
    screen_w: u16,
    screen_h: u16,
) -> Result<()> {
    let screen_h = screen_h.max(2);

    // Materialize the live inline cells once: streaming assistant followed
    // by any `Exploring`/`Running` tool-call placeholders. `inline_lines_capped`
    // already caps the row count at `MAX_STREAMING_ROWS` and prioritizes
    // placeholders over streaming-assistant tokens — without that, a long
    // streaming reply would push `Exploring`/`Running` rows past the cap
    // and they'd be invisibly clipped by ratatui's `Paragraph`.
    let streaming_lines = chat.inline_lines_capped(screen_w, MAX_STREAMING_ROWS);
    let streaming_h = streaming_lines.len() as u16;
    let max_composer = screen_h.saturating_sub(STATUS_ROWS + streaming_h).max(1);
    let composer_h = pane.desired_height(screen_w).max(3).min(max_composer);

    // Sticky-top viewport: preserve `viewport_area.top()` so the inline frame
    // doesn't slam against the bottom of the screen on every frame. On the
    // first frame this anchors the viewport just below the cursor's startup
    // row (where the shell prompt was), avoiding the "empty rows below the
    // viewport snapshotted into scrollback" leak. The viewport slides DOWN
    // naturally as `insert_history_lines` pushes content above it, and
    // bottom-anchors permanently once it reaches the screen floor.
    let old_area = terminal.viewport_area;
    let viewport_h = (streaming_h + composer_h + STATUS_ROWS).min(screen_h).max(1);
    let mut viewport_area = Rect::new(0, old_area.y, screen_w, viewport_h);

    // Expansion-overflow: if growing the viewport would push it past the
    // screen floor, scroll the rows directly above it into native scrollback
    // before slamming the viewport to bottom-anchored. This is what saves
    // the user-prompt row from being overwritten when streaming kicks in —
    // those rows are now in the terminal's scrollback instead of about to be
    // overpainted by the inline frame.
    if viewport_area.bottom() > screen_h {
        let scroll_by = viewport_area.bottom() - screen_h;
        if old_area.width > 0 && old_area.top() > 0 {
            crate::insert_history::scroll_region_above_into_scrollback(
                terminal,
                old_area.top(),
                scroll_by,
            )?;
        }
        viewport_area.y = screen_h - viewport_area.height;
    }

    // Blank rows the viewport is about to vacate (streaming cell finalized
    // or shrunk) before the next `insert_history_lines` scrolls them into
    // native scrollback as orphan fragments of a mid-stream paint.
    if old_area.width > 0 /* skip the pre-first-frame zero-sized area */
        && viewport_area.top() > old_area.top()
    {
        let backend = terminal.backend_mut();
        for row in old_area.top()..viewport_area.top() {
            queue!(backend, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        }
    }

    terminal.set_viewport_area(viewport_area);

    terminal.draw(|f| {
        let area = f.area();
        let chunks = Layout::vertical([
            Constraint::Length(streaming_h),
            Constraint::Length(composer_h),
            Constraint::Length(STATUS_ROWS),
        ])
        .split(area);

        if streaming_h > 0 {
            f.render_widget(Paragraph::new(streaming_lines), chunks[0]);
        }
        f.render_widget(pane, chunks[1]);
        if let Some((cx, cy)) = pane.cursor_position(chunks[1]) {
            f.set_cursor_position((cx, cy));
        }
        f.render_widget(
            StatusBar {
                model: status.model,
                cwd_short: status.cwd_short,
                branch: status.branch,
                dirty: status.dirty,
                state: status.state,
                tokens_input: status.tokens_input,
                context_window: status.context_window,
            },
            chunks[2],
        );
    })?;

    Ok(())
}
