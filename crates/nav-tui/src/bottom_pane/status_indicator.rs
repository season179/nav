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

use super::status_bar::{AgentState, StatusBarState, working_dots, working_pulse_color};

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
        let AgentState::Working { elapsed, spinner, tick } = self.state.agent_state else {
            return;
        };
        let secs = elapsed.as_secs();
        let dim = Style::default().fg(Color::DarkGray);
        let working = Style::default()
            .fg(working_pulse_color(tick))
            .add_modifier(Modifier::BOLD);
        let line = Line::from(vec![
            Span::styled("  ", dim),
            Span::styled(format!("{spinner} Working{} {secs}s", working_dots(tick)), working),
            Span::styled("  ·  Ctrl+C to interrupt", dim),
        ]);
        Paragraph::new(line).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use ratatui::layout::Position;

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

    fn working_state_at_tick(show: bool, tick: u64) -> StatusBarState {
        StatusBarState {
            agent_state: AgentState::Working {
                elapsed: Duration::from_secs(5),
                spinner: '⠴',
                tick,
            },
            show_indicator: show,
            ..StatusBarState::default()
        }
    }

    fn working_state(show: bool) -> StatusBarState {
        working_state_at_tick(show, 0)
    }

    #[test]
    fn renders_spinner_elapsed_and_interrupt_hint_when_visible() {
        let state = working_state(true);
        let mut b = buf(80);
        let widget = StatusIndicatorWidget { state: &state };
        widget.render(*b.area(), &mut b);
        let text = row_text(&b);
        assert!(text.contains("⠴ Working    5s"), "spinner+elapsed missing: {text:?}");
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

    #[test]
    fn animated_dots_cycle_through_phases() {
        // tick / 4 % 4:  0 → "",  4 → ".",  8 → "..",  12 → "..."
        let cases = [(0, "Working    5s"), (4, "Working.   5s"), (8, "Working..  5s"), (12, "Working... 5s")];
        for (tick, expected) in cases {
            let state = working_state_at_tick(true, tick);
            let mut b = buf(80);
            StatusIndicatorWidget { state: &state }.render(*b.area(), &mut b);
            let text = row_text(&b);
            assert!(
                text.contains(expected),
                "tick {tick}: expected {expected:?} in {text:?}"
            );
        }
    }

    #[test]
    fn color_pulses_between_magenta_shades() {
        // tick / 8 % 2 == 0 → Magenta, == 1 → LightMagenta
        let magenta_tick = 0u64;
        let light_tick = 8u64;
        let mut b1 = buf(80);
        StatusIndicatorWidget { state: &working_state_at_tick(true, magenta_tick) }
            .render(*b1.area(), &mut b1);
        let mut b2 = buf(80);
        StatusIndicatorWidget { state: &working_state_at_tick(true, light_tick) }
            .render(*b2.area(), &mut b2);
        // Find the 'W' of "Working" in both buffers and compare colours.
        let w1 = (0..b1.area().width).position(|x| b1.cell(Position::new(x, 0)).unwrap().symbol() == "W");
        let w2 = (0..b2.area().width).position(|x| b2.cell(Position::new(x, 0)).unwrap().symbol() == "W");
        assert!(w1.is_some(), "'W' not found at tick {magenta_tick}");
        assert!(w2.is_some(), "'W' not found at tick {light_tick}");
        let c1 = b1.cell(Position::new(w1.unwrap() as u16, 0)).unwrap().fg;
        let c2 = b2.cell(Position::new(w2.unwrap() as u16, 0)).unwrap().fg;
        assert_eq!(c1, Color::Magenta, "tick {magenta_tick} should be Magenta");
        assert_eq!(c2, Color::LightMagenta, "tick {light_tick} should be LightMagenta");
    }
}
