//! Render the pending follow-up queue above the composer.
//!
//! Sits in its own layout slot so the user can see what's queued without it
//! scrolling away with the rest of the transcript. The widget is intentionally
//! thin: it takes a borrow of [`PendingQueue`] previews and lays them out as
//! one row per item, plus a header.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::pending_input::QueuePreview;

/// Header height (one row) plus one row per queued item.
fn rows_for(previews: &[QueuePreview], steering_count: usize) -> u16 {
    if previews.is_empty() && steering_count == 0 {
        return 0;
    }
    // Cap the visible height so a runaway queue can't push the composer off
    // screen. Beyond the cap we collapse to a "+N more" footer row.
    const MAX_VISIBLE: usize = 6;
    let visible = previews.len().min(MAX_VISIBLE) as u16;
    let header = 1u16;
    let overflow = if previews.len() > MAX_VISIBLE { 1 } else { 0 };
    let steering = if steering_count > 0 { 1 } else { 0 };
    header
        .saturating_add(visible)
        .saturating_add(overflow)
        .saturating_add(steering)
}

/// Lightweight view over the queued items. Borrows so the queue itself stays
/// owned by the app loop.
pub struct PendingQueueView<'a> {
    previews: &'a [QueuePreview],
    /// Number of mid-turn steering messages that have been submitted but
    /// not yet drained by the runner. Rendered as a distinct row so the
    /// user can see hidden state that will inject "soon" rather than
    /// after the active turn settles.
    steering_count: usize,
}

impl<'a> PendingQueueView<'a> {
    pub fn new(previews: &'a [QueuePreview]) -> Self {
        Self {
            previews,
            steering_count: 0,
        }
    }

    /// Attach a steering-pending count. Builder-style so existing
    /// callers and snapshot tests don't have to spell out zero.
    pub fn with_steering(mut self, steering_count: usize) -> Self {
        self.steering_count = steering_count;
        self
    }

    /// Total rows needed for the current preview set, including the header.
    pub fn desired_height(&self) -> u16 {
        rows_for(self.previews, self.steering_count)
    }

    /// Owned [`Line`]s the widget will render. Public so snapshot tests can
    /// assert text content without driving the full ratatui buffer.
    pub fn lines(&self) -> Vec<Line<'static>> {
        if self.previews.is_empty() && self.steering_count == 0 {
            return Vec::new();
        }
        let accent = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);
        let dim = Style::default().fg(Color::DarkGray);
        let value = Style::default().fg(Color::White);
        let steering_accent = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD);

        let mut lines: Vec<Line<'static>> = Vec::new();
        let header = format!(
            "  ⋯ {} queued — Ctrl+E edit last · Ctrl+X clear · /steer …",
            self.previews.len()
        );
        lines.push(Line::from(Span::styled(header, accent)));

        const MAX_VISIBLE: usize = 6;
        for (idx, item) in self.previews.iter().take(MAX_VISIBLE).enumerate() {
            let mut spans: Vec<Span<'static>> = Vec::new();
            spans.push(Span::styled(format!("    #{} ", idx + 1), dim));
            if let Some(skill) = &item.skill {
                spans.push(Span::styled(
                    format!("/{skill} "),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if item.image_count > 0 {
                spans.push(Span::styled(
                    format!("[{}img] ", item.image_count),
                    Style::default().fg(Color::Cyan),
                ));
            }
            spans.push(Span::styled(item.summary.clone(), value));
            lines.push(Line::from(spans));
        }
        if self.previews.len() > MAX_VISIBLE {
            let hidden = self.previews.len() - MAX_VISIBLE;
            lines.push(Line::from(Span::styled(format!("    +{hidden} more"), dim)));
        }
        if self.steering_count > 0 {
            // Steering items live in a separate queue managed by the
            // runner. Show the count distinctly so the user can see
            // there's pending mid-turn input that will inject at the
            // next safe boundary — without surfacing payload text we
            // don't have ready to hand at this level.
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    ⮕ {} steering pending ", self.steering_count),
                    steering_accent,
                ),
                Span::styled("(injects at next model/tool boundary)", dim),
            ]));
        }
        lines
    }
}

impl<'a> Widget for PendingQueueView<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        if self.previews.is_empty() && self.steering_count == 0 {
            return;
        }
        let lines = self.lines();
        Paragraph::new(lines).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pending_input::PendingQueue;
    use std::path::PathBuf;

    fn skill(name: &str) -> crate::pending_input::QueuedSkill {
        crate::pending_input::QueuedSkill {
            name: name.into(),
            wrapped_body: format!("<skill name=\"{name}\">body</skill>"),
        }
    }

    #[test]
    fn empty_queue_renders_nothing_and_takes_no_rows() {
        let previews: Vec<QueuePreview> = Vec::new();
        let view = PendingQueueView::new(&previews);
        assert_eq!(view.desired_height(), 0);
        assert!(view.lines().is_empty());
    }

    #[test]
    fn renders_header_and_one_row_per_item() {
        let mut q = PendingQueue::new();
        q.enqueue("first".into(), vec![], None);
        q.enqueue(
            "second".into(),
            vec![PathBuf::from(".nav/clipboard/a.png")],
            Some(skill("foo")),
        );
        let previews = q.previews();
        let view = PendingQueueView::new(&previews);
        // Header + 2 rows.
        assert_eq!(view.desired_height(), 3);

        let rendered: Vec<String> = view
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.clone().into_owned())
                    .collect::<String>()
            })
            .collect();
        assert!(rendered[0].contains("2 queued"));
        assert!(rendered[1].contains("#1"));
        assert!(rendered[1].contains("first"));
        assert!(rendered[2].contains("#2"));
        assert!(rendered[2].contains("/foo"));
        assert!(rendered[2].contains("[1img]"));
        assert!(rendered[2].contains("second"));
    }

    #[test]
    fn collapses_overflow_into_more_row() {
        let mut q = PendingQueue::new();
        for i in 0..8 {
            q.enqueue(format!("item {i}"), vec![], None);
        }
        let previews = q.previews();
        let view = PendingQueueView::new(&previews);
        // Header + 6 visible + overflow row.
        assert_eq!(view.desired_height(), 8);

        let last_line: String = view
            .lines()
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.clone().into_owned())
            .collect();
        assert!(last_line.contains("+2 more"));
    }

    #[test]
    fn empty_follow_up_queue_still_renders_steering_pending_row() {
        let previews: Vec<QueuePreview> = Vec::new();
        let view = PendingQueueView::new(&previews).with_steering(2);
        // Header + steering row = 2 rows.
        assert_eq!(view.desired_height(), 2);
        let rendered: Vec<String> = view
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.clone().into_owned())
                    .collect::<String>()
            })
            .collect();
        assert!(rendered[0].contains("0 queued"));
        assert!(
            rendered[1].contains("2 steering pending"),
            "expected steering row, got {:?}",
            rendered[1]
        );
        assert!(rendered[1].contains("model/tool boundary"));
    }

    #[test]
    fn zero_steering_omits_steering_row() {
        let mut q = PendingQueue::new();
        q.enqueue("a".into(), vec![], None);
        let previews = q.previews();
        let view = PendingQueueView::new(&previews).with_steering(0);
        let rendered: Vec<String> = view
            .lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.clone().into_owned())
                    .collect::<String>()
            })
            .collect();
        for line in &rendered {
            assert!(
                !line.contains("steering pending"),
                "no steering row when count is zero: {line:?}"
            );
        }
    }

    #[test]
    fn renders_into_buffer_marks_header_and_item_indexes() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let mut q = PendingQueue::new();
        q.enqueue("alpha".into(), vec![], None);
        q.enqueue("bravo".into(), vec![], None);
        let previews = q.previews();
        let view = PendingQueueView::new(&previews);

        let area = Rect::new(0, 0, 60, view.desired_height());
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);
        let dump = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(dump[0].contains("2 queued"));
        assert!(dump[1].contains("#1"));
        assert!(dump[1].contains("alpha"));
        assert!(dump[2].contains("#2"));
        assert!(dump[2].contains("bravo"));
    }
}
