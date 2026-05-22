//! Inline viewport layout for the main TUI loop.
//!
//! Finalized chat history lives in the terminal's native scrollback (see
//! `crate::insert_history`), not inside ratatui. This module only paints
//! the inline viewport: any in-flight streaming text, the composer, and
//! the status bar. The viewport is sized per-frame and anchored to the
//! bottom of the screen.

use anyhow::Result;
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

    // Materialize the in-flight streaming cell once: one buffer walk yields
    // both the rendered lines and their height. Empty Vec when no stream
    // is active, so the streaming row collapses to zero height.
    let streaming_lines = chat.streaming_lines(screen_w);
    let streaming_h = (streaming_lines.len() as u16).min(MAX_STREAMING_ROWS);
    let max_composer = screen_h
        .saturating_sub(STATUS_ROWS + streaming_h)
        .max(1);
    let composer_h = pane
        .desired_height(screen_w)
        .max(3)
        .min(max_composer);

    let viewport_h = streaming_h + composer_h + STATUS_ROWS;
    let viewport_y = screen_h.saturating_sub(viewport_h);
    let viewport_area = Rect::new(0, viewport_y, screen_w, viewport_h);
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
