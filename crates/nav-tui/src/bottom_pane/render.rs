//! Layout math and rendering for the bottom pane.
//!
//! The pane stacks five chunks top-to-bottom:
//!
//! ```text
//! [status indicator] — 1 row, only while Working AND show_indicator
//! [overlay]          — variable (popup, approval, etc.)
//! [pending preview]  — variable (queued user inputs)
//! [composer]         — min 3 rows, flex (absorbs leftover)
//! [status bar]       — 1 row, always (bottom; matches codex)
//! ```
//!
//! The status bar is laid out separately: it is **always** reserved one
//! row at the bottom of the pane (the documented "always visible"
//! invariant). The remaining rows go to a [`FlexRenderable`] that stacks
//! indicator, overlay, pending preview, and composer. Composing status
//! through the same FlexRenderable would starve it under overlay
//! overflow — the allocator greedily fills non-flex children in push
//! order, so a tall slash popup would consume the status row before it
//! had a chance to claim its 1 row. Reserving status first preserves the
//! behaviour the previous `Layout::vertical` constraint solver gave us.
//!
//! Within the inner FlexRenderable, indicator/overlay/pending push with
//! `flex=0` (each reports its own exact desired height) and the composer
//! pushes with `flex=1` so any remaining rows go to it.
//!
//! `desired_height` and `cursor_position` use the same chunk heights and
//! the same status-row split so the pane's reported size, the rendered
//! rects, and the caret stay lockstep — drift between them is exactly
//! the class of bug the `tmux_viewport.rs` regression tests guard.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::pending_preview::{PendingPreview, render_pending_preview};
use super::status_bar::{StatusBar, StatusBarState};
use super::status_indicator::StatusIndicatorWidget;
use super::view::BottomPaneView;
use super::{BottomPane, GUTTER_WIDTH};
use crate::render::renderable::{FlexRenderable, Renderable, RenderableItem};
use crate::theme::Theme;

/// Height of the status-bar row at the bottom of the pane. The status bar
/// lives inside the pane (not as a peer chunk in `draw_tui`) so that all
/// "always visible" UI concentrates in one place. Placed at the bottom to
/// match codex's visual order (composer above, status below).
pub(super) const STATUS_ROWS: u16 = 1;

impl BottomPane {
    pub fn desired_height(&self, width: u16) -> u16 {
        let composer_h = self.composer_chunk_height(width);
        STATUS_ROWS
            .saturating_add(self.indicator_h())
            .saturating_add(composer_h)
            .saturating_add(self.overlay_h(width))
            .saturating_add(self.pending_preview_height())
    }

    /// Absolute screen position of the composer caret, given the rect the
    /// pane is being rendered into. Walks the same status-split +
    /// FlexRenderable layout used by `Widget::render` so the caret can't
    /// drift from the rendered composer.
    pub fn cursor_position(&self, pane_area: Rect) -> Option<(u16, u16)> {
        if pane_area.width == 0 || pane_area.height == 0 {
            return None;
        }
        let (inner, _status) = split_status(pane_area);
        let composer = ComposerChunk { pane: self };
        let flex = self.build_flex(inner.width, &composer);
        flex.cursor_pos(inner)
    }

    fn pending_preview_height(&self) -> u16 {
        if self.pending_inputs.is_empty() {
            0
        } else {
            1 + self.pending_inputs.len().min(super::pending_preview::VISIBLE_CAP) as u16
        }
    }

    /// 0 or 1, depending on whether the dedicated working-state row should
    /// occupy a layout slot. Single source of truth for the chunk size.
    fn indicator_h(&self) -> u16 {
        u16::from(StatusIndicatorWidget::is_visible(&self.status))
    }

    fn overlay_h(&self, width: u16) -> u16 {
        self.view
            .as_ref()
            .map(|v| v.desired_height(width))
            .unwrap_or(0)
    }

    /// Composer-chunk height (text rows plus the two padding rows above
    /// and below), clamped to a minimum of 3 so the filled background
    /// always reads as a distinct input block.
    fn composer_chunk_height(&self, width: u16) -> u16 {
        let content_w = width.saturating_sub(GUTTER_WIDTH);
        let composer_visual = self.composer.desired_height(content_w);
        composer_visual.saturating_add(2).max(3)
    }

    /// Build the FlexRenderable for the *non-status* rows
    /// (indicator/overlay/pending/composer). The status row is reserved
    /// separately by [`split_status`] before this is called. `composer`
    /// is borrowed in by the caller so the resulting FlexRenderable can
    /// outlive a temporary inside the impl.
    fn build_flex<'a>(
        &'a self,
        width: u16,
        composer: &'a ComposerChunk<'a>,
    ) -> FlexRenderable<'a> {
        let mut flex = FlexRenderable::new();
        flex.push(
            0,
            RenderableItem::Owned(Box::new(IndicatorChunk {
                state: &self.status,
                height: self.indicator_h(),
            })),
        );
        flex.push(
            0,
            RenderableItem::Owned(Box::new(OverlayChunk {
                view: self.view.as_deref(),
                height: self.overlay_h(width),
            })),
        );
        flex.push(
            0,
            RenderableItem::Owned(Box::new(PendingChunk {
                pending: &self.pending_inputs,
                theme: &self.theme,
                height: self.pending_preview_height(),
            })),
        );
        flex.push(1, RenderableItem::Borrowed(composer));
        flex
    }
}

/// Split the pane area into the inner stack and the bottom status row.
/// The status row is unconditionally reserved (when the pane has at
/// least one row at all) so a tall overlay can't starve it — this
/// matches the always-visible invariant the old `Layout::vertical`
/// solver preserved by ranking `Length(STATUS_ROWS)` against `Min(1)`.
fn split_status(area: Rect) -> (Rect, Rect) {
    if area.height == 0 {
        return (area, Rect { height: 0, ..area });
    }
    let status_h = STATUS_ROWS.min(area.height);
    let inner_h = area.height.saturating_sub(status_h);
    let inner = Rect {
        height: inner_h,
        ..area
    };
    let status_rect = Rect {
        y: area.y.saturating_add(inner_h),
        height: status_h,
        ..area
    };
    (inner, status_rect)
}

impl Widget for &BottomPane {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let (inner, status_rect) = split_status(area);
        let composer = ComposerChunk { pane: self };
        let flex = self.build_flex(inner.width, &composer);
        flex.render(inner, buf);
        if status_rect.height > 0 {
            StatusBar {
                state: &self.status,
            }
            .render(status_rect, buf);
        }
    }
}

/// Single-row working-state indicator. Reports zero height when not
/// visible so the row collapses cleanly.
struct IndicatorChunk<'a> {
    state: &'a StatusBarState,
    height: u16,
}

impl Renderable for IndicatorChunk<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        StatusIndicatorWidget { state: self.state }.render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.height
    }
}

/// Overlay slot — slash popup, mention popup, approval modal, session
/// picker. Height is whatever the overlay reports; zero when no overlay
/// is active.
struct OverlayChunk<'a> {
    view: Option<&'a dyn BottomPaneView>,
    height: u16,
}

impl Renderable for OverlayChunk<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if let Some(view) = self.view
            && area.height > 0
        {
            view.render(area, buf);
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.height
    }
}

/// Pending-input preview chip stack. Hidden when the queue is empty.
struct PendingChunk<'a> {
    pending: &'a [PendingPreview],
    theme: &'a Theme,
    height: u16,
}

impl Renderable for PendingChunk<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        render_pending_preview(self.pending, area, buf, self.theme);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.height
    }
}

/// Composer block + gutter + text area. Borrows the whole pane so it can
/// reach the composer, theme, and gutter constants without copying.
/// Reports its full desired height (text + padding) and gets `flex=1` so
/// it absorbs any remaining space in the pane area.
struct ComposerChunk<'a> {
    pane: &'a BottomPane,
}

impl Renderable for ComposerChunk<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        // Fill the entire composer block with the input background so the
        // gutter, padding rows and text all sit on the same coloured rect.
        let bg = Style::default().bg(self.pane.theme.composer_bg);
        Block::default().style(bg).render(area, buf);

        // One row of padding above and below the text so the block reads
        // as a distinct input region instead of butting up against the
        // status bar / overlay.
        let text_top = area.y.saturating_add(1);
        let text_rect = Rect {
            x: area.x,
            y: text_top,
            width: area.width,
            height: area.height.saturating_sub(2),
        };

        let [gutter, content] =
            Layout::horizontal([Constraint::Length(GUTTER_WIDTH), Constraint::Min(0)])
                .areas(text_rect);

        let prompt_style = if self.pane.composer.is_empty() {
            bg.fg(Color::DarkGray)
        } else {
            bg.fg(Color::White).add_modifier(Modifier::BOLD)
        };
        let prompt = Paragraph::new(Line::from(Span::styled("›", prompt_style))).style(bg);
        let gutter_first = Rect {
            height: 1.min(gutter.height),
            ..gutter
        };
        prompt.render(gutter_first, buf);
        self.pane.composer.render(content, buf, &self.pane.theme);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.pane.composer_chunk_height(width)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        // Composer paints into `area` with one row of top padding and a
        // gutter on the left. The caret sits at `(content_x + visual_col,
        // text_top + visual_row)`. When the chunk has been clipped to
        // less than 2 rows the text rect collapses and there's nowhere
        // for the caret to live.
        if area.height <= 1 {
            return None;
        }
        let text_top = area.y.saturating_add(1);
        let content_x = area.x.saturating_add(GUTTER_WIDTH);
        let content_width = area.width.saturating_sub(GUTTER_WIDTH);
        if content_width == 0 {
            return None;
        }
        let (vcol, vrow) = self.pane.composer.visual_position(content_width);
        Some((
            content_x.saturating_add(vcol),
            text_top.saturating_add(vrow),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bottom_pane::BottomPane;
    use crate::bottom_pane::status_bar::{AgentState, StatusBarState};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    /// Render the pane into a fresh buffer and return the row at the
    /// bottom of `area` as a plain string. Used to assert the status
    /// bar painted there.
    fn render_pane_bottom_row(pane: &BottomPane, area: Rect) -> String {
        let mut buf = Buffer::empty(area);
        Widget::render(pane, area, &mut buf);
        let last_y = area.y + area.height - 1;
        (area.x..area.x + area.width)
            .map(|x| buf[(x, last_y)].symbol().chars().next().unwrap_or(' '))
            .collect()
    }

    fn populated_status() -> StatusBarState {
        StatusBarState {
            model: "test-model".into(),
            cwd_short: "~/proj".into(),
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

    /// Regression for the status-bar-starvation bug surfaced in code
    /// review: when a slash popup's desired_height exceeds the available
    /// pane rows, FlexRenderable's greedy non-flex allocator used to
    /// consume the status row before status had a chance to claim it.
    /// `split_status` now reserves the row up front.
    #[test]
    fn status_row_survives_overlay_overflow() {
        let mut pane = BottomPane::new();
        pane.update_status(populated_status());
        // Open the slash popup — the default catalog has ~20 entries so
        // its desired_height is far larger than the 10-row area below.
        pane.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(pane.has_overlay(), "slash popup did not open");

        let area = Rect::new(0, 0, 60, 10);
        let bottom_row = render_pane_bottom_row(&pane, area);
        assert!(
            bottom_row.contains("Ready"),
            "status bar starved by overlay overflow — bottom row was {bottom_row:?}"
        );
    }

    /// Sanity: with plenty of vertical space the bottom row is still the
    /// status bar (i.e. the fix didn't move the row off-pane).
    #[test]
    fn status_row_sits_at_pane_bottom_in_normal_height() {
        let mut pane = BottomPane::new();
        pane.update_status(populated_status());
        let area = Rect::new(0, 0, 80, 24);
        let bottom_row = render_pane_bottom_row(&pane, area);
        assert!(
            bottom_row.contains("Ready"),
            "status bar missing at pane bottom in a tall pane — row was {bottom_row:?}"
        );
    }
}
