//! Bottom status bar.
//!
//! Renders a single row showing model, working directory, git branch, and
//! agent state (`Ready` or `Working …s`). Mirrors the codex CLI status row.
//!
//! The widget reads from `StatusBarState` held by [`super::BottomPane`]; the
//! main loop pushes new state in via [`super::BottomPane::update_status`] once
//! per draw cycle, and `BottomPane::render` paints this widget as the
//! bottommost row of the pane (below the composer), matching codex's layout.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use std::time::Duration;

/// Snapshot of what the status bar should show on the next draw. The main loop
/// assembles this once per frame and hands it to
/// [`super::BottomPane::update_status`]; the renderer reads from
/// [`super::BottomPane`]'s stored copy when painting.
#[derive(Debug, Clone)]
pub struct StatusBarState {
    pub model: String,
    pub cwd_short: String,
    pub branch: Option<String>,
    /// Append a yellow `✱` after the branch span when the worktree has
    /// uncommitted changes. False when not in a repo.
    pub dirty: bool,
    pub agent_state: AgentState,
    /// Latest provider-reported token counts for the status bar.
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_cached: u64,
    /// Effective context window used to compute the percentage. `0` hides the
    /// gauge entirely.
    pub context_window: u64,
    /// Allocate a dedicated row above the composer that shows the working
    /// spinner + interrupt hint. The main loop turns this on only when both
    /// the agent is in `Working` state AND `screen_h >=
    /// `super::status_indicator::INDICATOR_SCREEN_FLOOR``; the row stays
    /// hidden otherwise so small tmux splits don't lose composer space.
    ///
    /// When this is `true` the status bar suppresses its own
    /// `⠴ Working Ns` segment — the indicator row above is the canonical
    /// busy signal. The status bar's inline spinner is the fallback that
    /// kicks in when this flag is `false`.
    pub show_indicator: bool,
}

impl Default for StatusBarState {
    fn default() -> Self {
        Self {
            model: String::new(),
            cwd_short: String::new(),
            branch: None,
            dirty: false,
            agent_state: AgentState::Ready,
            tokens_input: 0,
            tokens_output: 0,
            tokens_cached: 0,
            context_window: 0,
            show_indicator: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AgentState {
    Ready,
    Working {
        elapsed: Duration,
        spinner: char,
        /// Raw spinner tick counter (~12 Hz). Used to derive animated dots
        /// and colour pulse without storing extra state in the enum.
        tick: u64,
    },
}

/// Animated dots suffix driven by the spinner tick counter.
/// Cycles `""` → `"."` → `".."` → `"..."` at ~3 Hz (tick rate ~12.5 Hz).
pub(super) fn working_dots(tick: u64) -> &'static str {
    match (tick / 4) % 4 {
        0 => "   ",
        1 => ".  ",
        2 => ".. ",
        _ => "...",
    }
}

/// Pulsing magenta colour driven by the spinner tick counter.
/// Alternates between `Magenta` and `LightMagenta` at ~1.5 Hz.
pub(super) fn working_pulse_color(tick: u64) -> Color {
    if (tick / 8) % 2 == 0 {
        Color::Magenta
    } else {
        Color::LightMagenta
    }
}

/// Gauge width in characters (exclusive of the leading space and brackets).
/// Total rendered width: 1 (space) + 1 (`[`) + 8 (inner) + 1 (`]`) = 11 chars.
const GAUGE_INNER: usize = 8;

/// Build a coloured gauge span like ` [████░░░░]` representing `pct`
/// percent fill. Returns `None` when `pct` is 0 (gauge hidden).
///
/// Colour thresholds: green < 50%, yellow 50–80%, red > 80%.
fn gauge_span(pct: u64) -> Option<Span<'static>> {
    if pct == 0 {
        return None;
    }
    let filled = ((pct.min(100) as usize * GAUGE_INNER).div_ceil(100)).min(GAUGE_INNER);
    let empty = GAUGE_INNER - filled;
    let color = if pct > 80 {
        Color::Red
    } else if pct >= 50 {
        Color::Yellow
    } else {
        Color::Green
    };
    Some(Span::styled(
        format!(" [{}{}]", "█".repeat(filled), "░".repeat(empty)),
        Style::default().fg(color),
    ))
}

/// Format `tokens` as `<n.n>k` (one decimal). Caller must gate on
/// `>= 1_000`; below that the gauge is hidden entirely.
fn format_tokens_k(tokens: u64) -> String {
    debug_assert!(tokens >= 1_000);
    let tenths = (tokens + 50) / 100;
    let whole = tenths / 10;
    let frac = tenths % 10;
    if frac == 0 {
        format!("{whole}k")
    } else {
        format!("{whole}.{frac}k")
    }
}

/// Renderable view over a [`StatusBarState`]. Owned by the pane; constructed
/// per-frame from the stored state.
pub(super) struct StatusBar<'a> {
    pub(super) state: &'a StatusBarState,
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let s = self.state;
        let dim = Style::default().fg(Color::DarkGray);
        let sep = Span::styled("  ·  ", dim);
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled("  ", dim),
            Span::styled(s.model.clone(), dim),
            sep.clone(),
            Span::styled(s.cwd_short.clone(), Style::default().fg(Color::Yellow)),
        ];
        if let Some(branch) = &s.branch {
            spans.push(sep.clone());
            spans.push(Span::styled(
                branch.clone(),
                Style::default().fg(Color::Green),
            ));
            if s.dirty {
                spans.push(Span::styled(
                    " ✱".to_string(),
                    Style::default().fg(Color::Yellow),
                ));
            }
        }
        // Agent-state segment. Suppressed when the dedicated indicator
        // row above the composer is carrying the working state — otherwise
        // "Working Ns" would render twice on screen. The inline spinner
        // still appears here when the indicator is hidden (small screens
        // below `INDICATOR_SCREEN_FLOOR`) so the user always has a busy
        // signal.
        match s.agent_state {
            AgentState::Ready => {
                spans.push(sep.clone());
                spans.push(Span::styled(
                    "Ready",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            AgentState::Working { elapsed, spinner, tick } if !s.show_indicator => {
                let secs = elapsed.as_secs();
                spans.push(sep.clone());
                spans.push(Span::styled(
                    format!("{spinner} Working{} {secs}s", working_dots(tick)),
                    Style::default().fg(working_pulse_color(tick)).add_modifier(Modifier::BOLD),
                ));
            }
            AgentState::Working { .. } => {
                // Indicator row carries the working state; skip the inline
                // segment so it isn't shown twice.
            }
        }
        // Token usage segment: show in/out/cached when we have data,
        // plus context-window percentage.
        if s.tokens_input >= 1_000 {
            spans.push(Span::styled("  ·  ", dim));
            let in_k = format_tokens_k(s.tokens_input);
            let out_display = if s.tokens_output >= 1_000 {
                format!(" ↑{}", format_tokens_k(s.tokens_output))
            } else {
                String::new()
            };
            let mut usage = format!("↓{}{}", in_k, out_display);
            if s.tokens_cached >= 1_000 {
                usage.push_str(&format!(" 💥{}", format_tokens_k(s.tokens_cached)));
            }
            spans.push(Span::styled(usage, dim));
            if let Some(pct) = s
                .tokens_input
                .saturating_mul(100)
                .checked_div(s.context_window)
            {
                let denom_k = (s.context_window + 500) / 1_000;
                spans.push(Span::styled(format!(" · {denom_k}k {pct}%"), dim));
                if let Some(gauge) = gauge_span(pct) {
                    spans.push(gauge);
                }
            }
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_hidden_when_pct_zero() {
        assert!(gauge_span(0).is_none());
    }

    #[test]
    fn gauge_appears_at_one_percent() {
        let bar = gauge_span(1).expect("should render at 1%");
        // 1% of 8 = 1 filled block.
        assert_eq!(bar.content, " [█░░░░░░░]");
        assert_eq!(bar.style.fg, Some(Color::Green));
    }

    #[test]
    fn gauge_green_below_50() {
        let bar = gauge_span(49).unwrap();
        assert_eq!(bar.style.fg, Some(Color::Green));
        // 49% of 8 = ceil(3.92) = 4 filled.
        assert_eq!(bar.content, " [████░░░░]");
    }

    #[test]
    fn gauge_yellow_at_50() {
        assert_eq!(gauge_span(50).unwrap().style.fg, Some(Color::Yellow));
    }

    #[test]
    fn gauge_yellow_up_to_80() {
        let bar = gauge_span(80).unwrap();
        assert_eq!(bar.style.fg, Some(Color::Yellow));
        // 80% of 8 = ceil(6.4) = 7 filled.
        assert_eq!(bar.content, " [███████░]");
    }

    #[test]
    fn gauge_red_above_80() {
        assert_eq!(gauge_span(81).unwrap().style.fg, Some(Color::Red));
    }

    #[test]
    fn gauge_full_at_100() {
        assert_eq!(gauge_span(100).unwrap().content, " [████████]");
    }

    #[test]
    fn gauge_clamped_above_100() {
        assert_eq!(gauge_span(200).unwrap().content, " [████████]");
    }

    /// Render the status bar into a buffer and return the resulting row as a
    /// plain string. Shared by the gauge-visibility end-to-end tests.
    fn render_status_row(context_window: u64) -> String {
        let state = StatusBarState {
            model: "m".into(),
            cwd_short: "~".into(),
            tokens_input: 50_000,
            context_window,
            ..StatusBarState::default()
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { state: &state }.render(area, &mut buf);
        (0..80)
            .map(|x| buf[(x, 0)].symbol().chars().next().unwrap_or(' '))
            .collect()
    }

    #[test]
    fn status_bar_renders_gauge_with_tokens() {
        let row = render_status_row(200_000);
        assert!(row.contains('█'), "gauge not found in: {row:?}");
        assert!(row.contains('░'), "gauge not found in: {row:?}");
    }

    #[test]
    fn status_bar_no_gauge_without_context_window() {
        let row = render_status_row(0);
        assert!(!row.contains('█'), "gauge should not appear: {row:?}");
    }
}
