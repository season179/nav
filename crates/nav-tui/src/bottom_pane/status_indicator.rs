//! Dedicated "Working …" status row rendered above the composer (and any
//! overlay) while a turn is active.
//!
//! Codex shows this row instead of squeezing the spinner into the
//! `Working Ns` segment of the status bar. While this row is visible the
//! status bar drops its inline `Working Ns` span so the working state
//! doesn't render twice — see [`super::status_bar::StatusBar::render`].
//! On small screens (below [`INDICATOR_SCREEN_FLOOR`]) this row is hidden
//! and the inline spinner reappears in the status bar as the fallback
//! busy signal.
//!
//! ## Small-screen floor
//!
//! Adding a row costs one row from the streaming preview during active
//! turns. On a 10-row tmux split that bite is painful, so the main loop
//! flips [`StatusBarState::show_indicator`] off below
//! [`INDICATOR_SCREEN_FLOOR`]. Both `BottomPane::desired_height` and the
//! render path check the same flag — keeping them lockstep prevents
//! resize artifacts (composer in the wrong row, blank indicator strip)
//! when crossing the threshold.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::status_bar::{AgentState, StatusBarState};

/// Screen-height threshold at or above which the dedicated indicator row is
/// shown. Below it the spinner stays inline in the status bar (current
/// behavior pre-#155). Read by the main loop when populating
/// [`StatusBarState::show_indicator`].
pub const INDICATOR_SCREEN_FLOOR: u16 = 12;

/// Renderable view over a [`StatusBarState`]. The widget is responsible
/// for the gating logic — callers always pass it the current state and
/// the widget paints nothing when `show_indicator` is false or the agent
/// is not in the `Working` state.
pub(super) struct StatusIndicatorWidget<'a> {
    pub(super) state: &'a StatusBarState,
}

impl<'a> StatusIndicatorWidget<'a> {
    /// True when the dedicated row should occupy a layout slot. Mirrored
    /// by `BottomPane::indicator_h` so layout math and render path agree.
    pub(super) fn is_visible(state: &StatusBarState) -> bool {
        state.show_indicator && matches!(state.agent_state, AgentState::Working { .. })
    }
}

impl<'a> Widget for StatusIndicatorWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || !Self::is_visible(self.state) {
            return;
        }
        let AgentState::Working { elapsed, spinner } = self.state.agent_state else {
            return;
        };
        let secs = elapsed.as_secs();
        let dim = Style::default().fg(Color::DarkGray);
        let working = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD);
        let line = Line::from(vec![
            Span::styled("  ", dim),
            Span::styled(format!("{spinner} Working {secs}s"), working),
            Span::styled("  ·  Ctrl+C to interrupt", dim),
        ]);
        Paragraph::new(line).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn buf(width: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, width, 1))
    }

    fn row_text(buf: &Buffer) -> String {
        let area = buf.area();
        (0..area.width)
            .map(|x| buf[(x, 0)].symbol())
            .collect::<Vec<_>>()
            .join("")
    }

    fn working_state(show: bool) -> StatusBarState {
        StatusBarState {
            agent_state: AgentState::Working {
                elapsed: Duration::from_secs(5),
                spinner: '⠴',
            },
            show_indicator: show,
            ..StatusBarState::default()
        }
    }

    #[test]
    fn renders_spinner_elapsed_and_interrupt_hint_when_visible() {
        let state = working_state(true);
        let mut b = buf(80);
        let widget = StatusIndicatorWidget { state: &state };
        widget.render(*b.area(), &mut b);
        let text = row_text(&b);
        assert!(text.contains("⠴ Working 5s"), "spinner+elapsed missing: {text:?}");
        assert!(
            text.contains("Ctrl+C to interrupt"),
            "interrupt hint missing: {text:?}"
        );
    }

    #[test]
    fn paints_nothing_when_show_indicator_is_off() {
        let state = working_state(false);
        let mut b = buf(80);
        let widget = StatusIndicatorWidget { state: &state };
        widget.render(*b.area(), &mut b);
        // Empty buffer means every cell is the default ' ' symbol.
        assert!(row_text(&b).trim().is_empty(), "expected blank row");
    }

    #[test]
    fn paints_nothing_when_agent_is_ready_even_if_flag_is_on() {
        let state = StatusBarState {
            agent_state: AgentState::Ready,
            show_indicator: true,
            ..StatusBarState::default()
        };
        let mut b = buf(80);
        let widget = StatusIndicatorWidget { state: &state };
        widget.render(*b.area(), &mut b);
        assert!(row_text(&b).trim().is_empty(), "expected blank row when Ready");
    }

    #[test]
    fn is_visible_matches_both_conditions() {
        assert!(StatusIndicatorWidget::is_visible(&working_state(true)));
        assert!(!StatusIndicatorWidget::is_visible(&working_state(false)));
        assert!(!StatusIndicatorWidget::is_visible(&StatusBarState::default()));
    }
}
