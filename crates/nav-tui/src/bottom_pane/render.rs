//! Layout math and rendering for the bottom pane.
//!
//! The pane stacks five chunks top-to-bottom:
//!
//! ```text
//! [status bar]       — 1 row, always
//! [status indicator] — 1 row, only while Working AND show_indicator
//! [overlay]          — variable (popup, approval, etc.)
//! [pending preview]  — variable (queued user inputs)
//! [composer]         — min 3 rows
//! ```
//!
//! The indicator slot has zero height when the gating flag is off, so the
//! row collapses cleanly without leaving a blank strip. `desired_height`,
//! `cursor_position`, and the render-time `Layout::vertical` use the same
//! `indicator_h()` helper — keeping them lockstep prevents drift between
//! "how tall the pane wants to be" and "where the composer caret actually
//! lands."

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};

use super::pending_preview::render_pending_preview;
use super::status_bar::StatusBar;
use super::status_indicator::StatusIndicatorWidget;
use super::{BottomPane, GUTTER_WIDTH};

/// Height of the status-bar row at the top of the pane. The status bar lives
/// inside the pane (not as a peer chunk in `draw_tui`) so that all "always
/// visible" UI concentrates in one place.
pub(super) const STATUS_ROWS: u16 = 1;

impl BottomPane {
    pub fn desired_height(&self, width: u16) -> u16 {
        // Composer always reserves at least 3 rows so the filled background
        // reads as a distinct input block (one row of `›` + text plus a row
        // of padding above and below — matches the codex visual weight).
        let content_w = width.saturating_sub(GUTTER_WIDTH);
        let composer_visual = self.composer.desired_height(content_w);
        let composer_h = composer_visual.saturating_add(2).max(3);
        let overlay_h = self
            .view
            .as_ref()
            .map(|v| v.desired_height(width))
            .unwrap_or(0);
        STATUS_ROWS
            .saturating_add(self.indicator_h())
            .saturating_add(composer_h)
            .saturating_add(overlay_h)
            .saturating_add(self.pending_preview_height())
    }

    /// Absolute screen position of the composer caret, given the rect the
    /// pane is being rendered into. Mirrors the layout in [`Widget::render`].
    pub fn cursor_position(&self, pane_area: Rect) -> Option<(u16, u16)> {
        if pane_area.width == 0 || pane_area.height == 0 {
            return None;
        }
        let overlay_h = self
            .view
            .as_ref()
            .map(|v| v.desired_height(pane_area.width))
            .unwrap_or(0);
        let queue_h = self.pending_preview_height();
        let indicator_h = self.indicator_h();
        // The status row sits at the top of the pane; the optional
        // indicator row sits below it; everything else shifts down by
        // both. Without these offsets the caret lands inside the status
        // bar or the indicator strip instead of the composer.
        let composer_y = pane_area
            .y
            .saturating_add(STATUS_ROWS)
            .saturating_add(indicator_h)
            .saturating_add(overlay_h)
            .saturating_add(queue_h);
        let composer_h = pane_area
            .height
            .saturating_sub(STATUS_ROWS)
            .saturating_sub(indicator_h)
            .saturating_sub(overlay_h)
            .saturating_sub(queue_h);
        if composer_h <= 1 {
            return None;
        }
        let text_top = composer_y.saturating_add(1);
        let content_x = pane_area.x.saturating_add(GUTTER_WIDTH);
        let content_width = pane_area.width.saturating_sub(GUTTER_WIDTH);
        if content_width == 0 {
            return None;
        }
        let (vcol, vrow) = self.composer.visual_position(content_width);
        Some((
            content_x.saturating_add(vcol),
            text_top.saturating_add(vrow),
        ))
    }

    fn pending_preview_height(&self) -> u16 {
        if self.pending_inputs.is_empty() {
            0
        } else {
            1 + self.pending_inputs.len().min(4) as u16
        }
    }

    /// 0 or 1, depending on whether the dedicated working-state row should
    /// occupy a layout slot. Single source of truth for the chunk size.
    fn indicator_h(&self) -> u16 {
        if StatusIndicatorWidget::is_visible(&self.status) {
            1
        } else {
            0
        }
    }
}

impl Widget for &BottomPane {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let overlay_h = self
            .view
            .as_ref()
            .map(|v| v.desired_height(area.width))
            .unwrap_or(0);
        let queue_h = self.pending_preview_height();
        let indicator_h = self.indicator_h();
        let [status_rect, indicator_rect, overlay_rect, queue_rect, composer_outer] =
            Layout::vertical([
                Constraint::Length(STATUS_ROWS),
                Constraint::Length(indicator_h),
                Constraint::Length(overlay_h),
                Constraint::Length(queue_h),
                Constraint::Min(1),
            ])
            .areas(area);

        if status_rect.height > 0 {
            StatusBar {
                state: &self.status,
            }
            .render(status_rect, buf);
        }

        if indicator_rect.height > 0 {
            StatusIndicatorWidget {
                state: &self.status,
            }
            .render(indicator_rect, buf);
        }

        if let Some(view) = self.view.as_ref()
            && overlay_rect.height > 0
        {
            view.render(overlay_rect, buf);
        }

        if queue_rect.height > 0 {
            render_pending_preview(&self.pending_inputs, queue_rect, buf, &self.theme);
        }

        if composer_outer.height > 0 {
            // Fill the entire composer block with the input background so the
            // gutter, padding rows and text all sit on the same coloured rect.
            let bg = Style::default().bg(self.theme.composer_bg);
            Block::default().style(bg).render(composer_outer, buf);

            // One row of padding above and below the text so the block reads
            // as a distinct input region instead of butting up against the
            // status bar / overlay.
            let text_top = composer_outer.y.saturating_add(1);
            let text_rect = Rect {
                x: composer_outer.x,
                y: text_top,
                width: composer_outer.width,
                height: composer_outer.height.saturating_sub(2),
            };

            let [gutter, content] =
                Layout::horizontal([Constraint::Length(GUTTER_WIDTH), Constraint::Min(0)])
                    .areas(text_rect);

            let prompt_style = if self.composer.is_empty() {
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
            self.composer.render(content, buf, &self.theme);
        }
    }
}
