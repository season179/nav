//! Screen layout for the main TUI loop.

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use std::io::Stdout;

use crate::ChatWidget;
use crate::bottom_pane;

use super::status_bar::{AgentState, StatusBar};

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
) -> Result<(u16, u16)> {
    let mut history_viewport = (1, 1);
    terminal.draw(|f| {
        let area = f.area();
        let pane_h = pane
            .desired_height(area.width)
            .max(3)
            .min(area.height.saturating_sub(2));
        let chunks = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(pane_h),
            Constraint::Length(1),
        ])
        .split(area);
        history_viewport = (chunks[0].width, chunks[0].height);
        f.render_widget(chat, chunks[0]);
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
    Ok(history_viewport)
}
