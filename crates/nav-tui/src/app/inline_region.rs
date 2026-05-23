//! Pure viewport-boundary math for the inline TUI frame.
//!
//! `draw_tui` is split in two: this module decides *what* the new viewport
//! rect should be (and what side-effects must accompany the resize), and
//! `draw_tui` performs the side-effects + widget rendering. Keeping the math
//! pure lets us unit-test the edge cases (small screens, expansion overflow,
//! shrink-blanking) without a terminal backend.
//!
//! All side-effects are surfaced as data on [`InlineRegion`]:
//!   - `scroll_above` — when set, the caller must call
//!     `insert_history::scroll_region_above_into_scrollback` *before*
//!     `set_viewport_area`.
//!   - `blank_rows` — when set, the caller must erase those rows on the
//!     backend *before* `set_viewport_area`.
//!
//! See `draw_tui` for the canonical sequence.
//!
//! Constants:
//!   - [`MAX_STREAMING_ROWS`] caps the in-flight streaming preview so a
//!     long reply can't shove the composer off-screen.
//!   - [`MIN_PANE_ROWS`] is the bottom-pane floor (1 status + 3 composer).
//!   - [`SCROLLBACK_RESERVE`] keeps two rows above the viewport for native
//!     scrollback insertion; without it `insert_history_lines` emits an
//!     invalid DECSTBM range and the inline frame can be overpainted.

use std::ops::Range;

use ratatui::layout::Rect;

/// Cap the streaming preview at this many rows so a long in-flight reply
/// can't shove the composer off-screen. Once the reply finalizes it goes
/// to scrollback and the cap stops mattering.
pub(super) const MAX_STREAMING_ROWS: u16 = 16;

/// Bottom-pane minimum height: status (1) + composer floor (3). Used as a
/// budget when computing how much room is left for the streaming preview
/// and as a floor on the rendered pane height.
pub(super) const MIN_PANE_ROWS: u16 = 4;

/// Reserve this many rows above the inline viewport for native scrollback
/// insertion. `insert_history_lines` defines its upper scroll region as
/// `SetScrollRegion(1..area.top())`, which is only a valid DECSTBM when
/// `area.top() >= 2` (top < bottom, 1-based). Without this clamp, a
/// streaming preview tall enough to fill the screen drives `area.y` to 0
/// via the overflow branch, and the next history flush emits
/// `\x1b[1;0r`; several terminals fall back to a full-screen region on
/// that invalid range and the history rows then overpaint the inline
/// frame.
pub(super) const SCROLLBACK_RESERVE: u16 = 2;

/// Clamp the raw screen height to a value the rest of the math can rely
/// on. The minimum of 2 keeps DECSTBM valid even on degenerate terminals.
fn clamp_screen_h(screen_h: u16) -> u16 {
    screen_h.max(2)
}

/// Cap for `ChatWidget::inline_lines_capped`. Materializing the streaming
/// lines is a separate concern from computing the rect, but the cap depends
/// on the same screen budget so it lives here.
pub(super) fn streaming_cap(screen_h: u16) -> u16 {
    let max_inline = clamp_screen_h(screen_h)
        .saturating_sub(SCROLLBACK_RESERVE)
        .max(1);
    MAX_STREAMING_ROWS.min(max_inline.saturating_sub(MIN_PANE_ROWS))
}

/// Rows above the previous viewport that the caller must slide into native
/// scrollback before resizing the inline frame. Emitted only when the new
/// viewport would extend past the screen floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScrollAbove {
    pub(super) top: u16,
    pub(super) by: u16,
}

/// Result of one frame's viewport layout. Fields are pure data; the caller
/// (`draw_tui`) drives all side-effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InlineRegion {
    /// Rect to hand to `Terminal::set_viewport_area`.
    pub(super) viewport_area: Rect,
    /// Streaming chunk height (rows used for in-flight reply + tool
    /// placeholders). Mirrors `streaming_lines.len()` after capping.
    pub(super) streaming_h: u16,
    /// Bottom-pane chunk height.
    pub(super) pane_h: u16,
    /// `Some` when the new viewport overflows the screen floor and the
    /// caller must scroll the rows above the *old* viewport into native
    /// scrollback before resizing. `None` on first frame (no old rect),
    /// when `old_area.top() == 0`, or when no overflow occurred.
    pub(super) scroll_above: Option<ScrollAbove>,
    /// `Some(start..end)` when the new viewport is shorter than the old one
    /// and the rows it vacated at the bottom must be erased on the backend
    /// before the resize. `None` when the viewport grew, didn't move, or
    /// the previous frame was zero-sized.
    pub(super) blank_rows: Option<Range<u16>>,
}

impl InlineRegion {
    /// Compute the viewport layout for one frame.
    ///
    /// `streaming_h` is the materialized streaming-line count (after
    /// `streaming_cap` was applied to materialize them). `pane_desired_h`
    /// is `BottomPane::desired_height(screen_w)`. `old_area` is the
    /// terminal's previous `viewport_area` (zero-sized on the first frame).
    pub(super) fn compute(
        screen_w: u16,
        screen_h: u16,
        streaming_h: u16,
        pane_desired_h: u16,
        old_area: Rect,
    ) -> Self {
        let screen_h = clamp_screen_h(screen_h);
        let max_inline = screen_h.saturating_sub(SCROLLBACK_RESERVE).max(1);

        // Clamp streaming to whatever the cap allows in case the caller
        // passed in a count larger than the materializer would have
        // produced (e.g. tests).
        let cap = MAX_STREAMING_ROWS.min(max_inline.saturating_sub(MIN_PANE_ROWS));
        let streaming_h = streaming_h.min(cap);

        let max_pane = max_inline.saturating_sub(streaming_h).max(1);
        let pane_h = pane_desired_h.max(MIN_PANE_ROWS).min(max_pane);

        // Sticky-top viewport: preserve `old_area.y` so the inline frame
        // doesn't slam against the screen floor on every frame. On the
        // first frame `old_area.y` is the cursor row at startup, which
        // anchors the viewport just below the shell prompt.
        let viewport_h = (streaming_h + pane_h).min(max_inline).max(1);
        let mut viewport_area = Rect::new(0, old_area.y, screen_w, viewport_h);

        // Expansion-overflow: if the new viewport would push past the
        // screen floor, scroll the rows above the *old* viewport into
        // native scrollback first, then bottom-anchor. The anchor must
        // happen even when the scroll call is skipped (no old rect /
        // old_area.top() == 0); without that, viewport_area.y would
        // exceed screen_h and the frame would be invisible.
        let scroll_above = if viewport_area.bottom() > screen_h {
            let scroll_by = viewport_area.bottom() - screen_h;
            viewport_area.y = screen_h - viewport_area.height;
            if old_area.width > 0 && old_area.top() > 0 {
                Some(ScrollAbove {
                    top: old_area.top(),
                    by: scroll_by,
                })
            } else {
                None
            }
        } else {
            None
        };

        // Shrink-blank: if the new viewport is shorter than the old, the
        // rows it vacated at the bottom still hold the previous frame's
        // pixels (e.g. stale streaming text below the new composer).
        // Caller must erase them before the resize.
        let blank_rows = if old_area.width > 0 && viewport_area.bottom() < old_area.bottom() {
            let start = viewport_area.bottom().max(old_area.top());
            let end = old_area.bottom().min(screen_h);
            if start < end { Some(start..end) } else { None }
        } else {
            None
        };

        InlineRegion {
            viewport_area,
            streaming_h,
            pane_h,
            scroll_above,
            blank_rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_at(y: u16, h: u16, w: u16) -> Rect {
        Rect::new(0, y, w, h)
    }

    #[test]
    fn empty_streaming_fits_inside_screen_without_overflow() {
        // Typical 80×24 with no streaming and a small pane: the viewport
        // sticks to its previous y and grows by exactly pane_h rows.
        let region = InlineRegion::compute(80, 24, 0, 4, old_at(20, 4, 80));
        assert_eq!(region.streaming_h, 0);
        assert_eq!(region.pane_h, 4);
        assert_eq!(region.viewport_area, Rect::new(0, 20, 80, 4));
        assert_eq!(region.scroll_above, None);
        assert_eq!(region.blank_rows, None);
    }

    #[test]
    fn streaming_plus_pane_overflowing_screen_floor_triggers_scroll_above() {
        // Old viewport sits 20 rows down on a 24-row screen; new viewport
        // wants streaming(5) + pane(4) = 9 rows, which overflows the floor
        // by 5. Those 5 rows must scroll into scrollback from the *old*
        // top, and the viewport must bottom-anchor.
        let old = old_at(20, 4, 80);
        let region = InlineRegion::compute(80, 24, 5, 4, old);
        assert_eq!(region.streaming_h, 5);
        assert_eq!(region.pane_h, 4);
        assert_eq!(region.viewport_area.height, 9);
        assert_eq!(region.viewport_area.y, 24 - 9, "must bottom-anchor on overflow");
        assert_eq!(
            region.scroll_above,
            Some(ScrollAbove { top: 20, by: 5 }),
            "must surface the rows-to-scroll count"
        );
        assert_eq!(region.blank_rows, None);
    }

    #[test]
    fn overflow_without_old_rect_still_anchors_but_skips_scroll() {
        // First frame: old_area is zero-sized. We can't scroll a
        // nonexistent region above, but the viewport must still
        // bottom-anchor or it'd be invisible.
        let old = Rect::new(0, 0, 0, 0);
        let region = InlineRegion::compute(80, 24, 12, 6, old);
        assert!(region.viewport_area.bottom() <= 24);
        assert_eq!(
            region.scroll_above, None,
            "no scrollable rows when there's no previous viewport"
        );
    }

    #[test]
    fn shrinking_viewport_reports_rows_to_blank() {
        // Old viewport was 8 rows tall sitting at y=14; new viewport
        // shrinks to 4 rows but keeps y=14. Rows 18..22 vacated at the
        // bottom must be reported so the caller can erase the stale
        // streaming pixels there.
        let old = old_at(14, 8, 80);
        let region = InlineRegion::compute(80, 24, 0, 4, old);
        assert_eq!(region.viewport_area, Rect::new(0, 14, 80, 4));
        assert_eq!(region.blank_rows, Some(18..22));
        assert_eq!(region.scroll_above, None);
    }

    #[test]
    fn pane_height_floors_to_min_pane_rows() {
        // A pane that says it only wants 1 row still gets MIN_PANE_ROWS,
        // otherwise the status row would clip onto the composer floor.
        let region = InlineRegion::compute(80, 24, 0, 1, old_at(0, 0, 0));
        assert_eq!(region.pane_h, MIN_PANE_ROWS);
    }

    #[test]
    fn pane_height_clamps_to_available_room() {
        // A pane that wants 100 rows on a 24-row screen gets clipped to
        // max_inline (22 = 24 − SCROLLBACK_RESERVE), not MIN_PANE_ROWS.
        let region = InlineRegion::compute(80, 24, 0, 100, old_at(0, 0, 0));
        assert!(region.pane_h <= 22);
        assert!(region.pane_h >= MIN_PANE_ROWS);
    }

    #[test]
    fn tiny_screen_clamps_to_minimum_height() {
        // A 1-row screen is clamped to 2 internally; max_inline ends up
        // at 1 (after SCROLLBACK_RESERVE saturating_sub), no room for
        // streaming, and the pane is squished to 1 row. The frame stays
        // valid (height >= 1) rather than collapsing to zero.
        let region = InlineRegion::compute(80, 1, 0, 4, old_at(0, 0, 0));
        assert_eq!(region.streaming_h, 0);
        assert!(region.viewport_area.height >= 1);
    }

    #[test]
    fn streaming_count_above_cap_is_clamped() {
        // Caller passed in 50 streaming rows but the cap on a 24-row
        // screen is MAX_STREAMING_ROWS (16) min (22 − 4) = 16. The
        // returned streaming_h must be 16, not 50.
        let region = InlineRegion::compute(80, 24, 50, 4, old_at(0, 0, 0));
        assert_eq!(region.streaming_h, MAX_STREAMING_ROWS);
    }

    #[test]
    fn streaming_cap_helper_matches_internal_clamping() {
        // The pre-materialize cap and the in-compute clamp must agree —
        // otherwise the caller would pass in more rows than the region
        // would accept and the extras would be silently dropped at
        // resize time.
        for screen_h in [2u16, 5, 10, 24, 80, 1000] {
            let cap = streaming_cap(screen_h);
            let region = InlineRegion::compute(80, screen_h, cap, 4, old_at(0, 0, 0));
            assert_eq!(region.streaming_h, cap, "screen_h={screen_h}");
        }
    }
}
