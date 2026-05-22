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

/// Composer minimum height — keeps at least one input row plus padding.
const MIN_COMPOSER_ROWS: u16 = 3;

/// Reserve this many rows above the inline viewport for native scrollback
/// insertion. `insert_history_lines` defines its upper scroll region as
/// `SetScrollRegion(1..area.top())`, which is only a valid DECSTBM when
/// `area.top() >= 2` (top < bottom, 1-based). Without this clamp, a
/// streaming preview tall enough to fill the screen drives `area.y` to 0
/// via the overflow branch below, and the next history flush emits
/// `\x1b[1;0r`; several terminals fall back to a full-screen region on
/// that invalid range and the history rows then overpaint the inline
/// frame.
const SCROLLBACK_RESERVE: u16 = 2;

pub(super) struct TuiStatus<'a> {
    pub model: &'a str,
    pub cwd_short: &'a str,
    pub branch: Option<&'a str>,
    pub dirty: bool,
    pub state: AgentState,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_cached: u64,
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

    // Inline frame fits in `max_inline` rows so that at least
    // `SCROLLBACK_RESERVE` rows always remain above the viewport for
    // native scrollback insertion (see the constant's doc comment).
    let max_inline = screen_h.saturating_sub(SCROLLBACK_RESERVE).max(1);

    // Materialize the live inline cells once: streaming assistant followed
    // by any `Exploring`/`Running` tool-call placeholders. `inline_lines_capped`
    // already caps the row count at `MAX_STREAMING_ROWS` and prioritizes
    // placeholders over streaming-assistant tokens — without that, a long
    // streaming reply would push `Exploring`/`Running` rows past the cap
    // and they'd be invisibly clipped by ratatui's `Paragraph`. Tighten the
    // cap further on small terminals so composer + status still fit inside
    // `max_inline`.
    let streaming_cap = MAX_STREAMING_ROWS
        .min(max_inline.saturating_sub(STATUS_ROWS + MIN_COMPOSER_ROWS));
    let streaming_lines = chat.inline_lines_capped(screen_w, streaming_cap);
    let streaming_h = streaming_lines.len() as u16;
    let max_composer = max_inline.saturating_sub(STATUS_ROWS + streaming_h).max(1);
    let composer_h = pane
        .desired_height(screen_w)
        .max(MIN_COMPOSER_ROWS)
        .min(max_composer);

    // Sticky-top viewport: preserve `viewport_area.top()` so the inline frame
    // doesn't slam against the bottom of the screen on every frame. On the
    // first frame this anchors the viewport just below the cursor's startup
    // row (where the shell prompt was), avoiding the "empty rows below the
    // viewport snapshotted into scrollback" leak.
    //
    // When the viewport shrinks (e.g. a streaming assistant cell finalizes),
    // area.y stays put and the height drops at the bottom. The caller pairs
    // this resize with a follow-up `insert_history_lines` that slides the
    // now-smaller viewport DOWN by the freed rows below it — re-anchoring
    // the composer at the screen floor without leaving a blank band above.
    let old_area = terminal.viewport_area;
    let viewport_h = (streaming_h + composer_h + STATUS_ROWS).min(max_inline).max(1);
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

    // Blank rows the new viewport vacates at the bottom (e.g. streaming
    // cell finalized and the inline frame collapsed back to composer +
    // status with `area.y` preserved). Without this clear, the stale
    // streaming text painted into those rows on the previous frame stays
    // on screen below the new composer, looking like the message got
    // duplicated. `old_area.width == 0` only on the pre-first-frame
    // zero-sized area — skip then.
    if old_area.width > 0 && viewport_area.bottom() < old_area.bottom() {
        let backend = terminal.backend_mut();
        let start = viewport_area.bottom().max(old_area.top());
        let end = old_area.bottom().min(screen_h);
        for row in start..end {
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
                tokens_output: status.tokens_output,
                tokens_cached: status.tokens_cached,
                context_window: status.context_window,
            },
            chunks[2],
        );
    })?;

    Ok(())
}
