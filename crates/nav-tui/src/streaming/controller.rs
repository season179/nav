//! Stream-controller layer for assistant reply rendering.
//!
//! The controller keeps the raw streamed source, derives stable vs tail ranges
//! based on markdown fences/table rules, and pushes newly emitted lines into a
//! FIFO queue consumed by commit-tick scheduling.

use std::time::{Duration, Instant};

use crate::cells::{count_wrapped_body_lines, render_body};
use ratatui::text::Line;

use super::StreamState;

/// Cached width used for queue refreshes and fallback line rendering.
const DEFAULT_STREAM_WIDTH: u16 = 80;

#[derive(Default)]
pub(crate) struct StreamController {
    collector: MarkdownStreamCollector,
    stream_state: StreamState,
    finalized: bool,
    render_width: u16,
    rendered_cache: Vec<Line<'static>>,
}

#[derive(Default)]
struct MarkdownStreamCollector {
    text: String,
}

impl MarkdownStreamCollector {
    fn push(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn replace(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
    }

    fn content(&self) -> &str {
        &self.text
    }
}

impl StreamController {
    pub(crate) fn set_width(&mut self, width: u16) {
        self.render_width = width.max(1);
        self.requeue_lines();
    }

    /// Append `text` to the running stream source.
    pub(crate) fn push_delta(&mut self, text: &str) {
        if self.finalized {
            return;
        }

        self.collector.push(text);
        self.stream_state.has_seen_delta = true;
        self.requeue_lines();
    }

    /// Replace the streamed source with `text`, preserving finalize semantics.
    pub(crate) fn replace_buffer(&mut self, text: &str) {
        self.collector.replace(text);
        self.stream_state.clear_queue();
        self.stream_state.has_seen_delta = true;
        self.rendered_cache.clear();
        self.finalized = false;
        self.requeue_lines();
    }

    /// Mark stream complete and flush whatever remains stable-aware.
    pub(crate) fn finalize(&mut self) {
        self.finalized = true;
        self.requeue_lines();
    }

    /// Consume one queued render line.
    pub(crate) fn on_commit_tick(&mut self) -> Vec<Line<'static>> {
        self.stream_state.step()
    }

    /// Consume up to `max_lines` queued render lines.
    pub(crate) fn on_commit_tick_batch(&mut self, max_lines: usize) -> Vec<Line<'static>> {
        self.stream_state.drain_n(max_lines)
    }

    /// Returns `true` when no lines are queued for commit.
    pub(crate) fn is_idle(&self) -> bool {
        self.stream_state.is_idle()
    }

    /// Number of queued lines pending commit.
    pub(crate) fn queued_lines(&self) -> usize {
        self.stream_state.queued_len()
    }

    /// Age of oldest queued line.
    pub(crate) fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.stream_state.oldest_queued_age(now)
    }

    pub(crate) fn stable_lines(&self, width: u16) -> Vec<Line<'static>> {
        let partition_end = self.partition_offset();
        render_body(&self.collector.content()[..partition_end], width)
    }

    pub(crate) fn tail_lines(&self, width: u16) -> Vec<Line<'static>> {
        let partition_end = self.partition_offset();
        render_body(&self.collector.content()[partition_end..], width)
    }

    /// Render both stable + tail in one pass.
    pub(crate) fn partitioned_lines(&self, width: u16) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        let partition_end = self.partition_offset();
        (
            render_body(&self.collector.content()[..partition_end], width),
            render_body(&self.collector.content()[partition_end..], width),
        )
    }

    /// Count partitioned line count without extra allocations.
    pub(crate) fn partitioned_line_count(&self, width: u16) -> usize {
        let partition_end = self.partition_offset();
        count_wrapped_body_lines(&self.collector.content()[..partition_end], width)
            + count_wrapped_body_lines(&self.collector.content()[partition_end..], width)
    }

    fn rendered_lines_at(&self, width: u16) -> Vec<Line<'static>> {
        let partition_end = self.partition_offset();
        let stable = render_body(&self.collector.content()[..partition_end], width);
        let mut lines = stable;
        lines.extend(render_body(&self.collector.content()[partition_end..], width));
        lines
    }

    fn requeue_lines(&mut self) {
        if !self.stream_state.has_seen_delta {
            self.stream_state.clear_queue();
            self.rendered_cache.clear();
            return;
        }

        let current = self.rendered_lines_at(self.render_width);

        let divergence = first_diverging_line(&self.rendered_cache, &current);
        let should_reset_queue = divergence < self.rendered_cache.len() || self.rendered_cache.is_empty();
        if should_reset_queue {
            self.stream_state.clear_queue();
        }
        self.stream_state.enqueue(current[divergence..].to_vec());

        self.rendered_cache = current;
    }

    fn partition_offset(&self) -> usize {
        if self.finalized {
            return self.collector.content().len();
        }

        let mut state = State::Outside;
        let mut last_safe: usize = 0;

        for span in line_spans(self.collector.content()) {
            match state {
                State::Outside => {
                    if !span.has_newline {
                        break;
                    }

                    if let Some(delim) = fence_delim(span.text) {
                        state = State::Fence(delim);
                    } else if is_table_row(span.text) {
                        state = State::Table;
                    } else {
                        last_safe = span.end;
                    }
                }
                State::Fence(delim) => {
                    if !span.has_newline {
                        break;
                    }
                    if is_fence_close(span.text, delim) {
                        last_safe = span.end;
                        state = State::Outside;
                    }
                }
                State::Table => {
                    if !span.has_newline {
                        break;
                    }

                    if is_table_row(span.text) {
                        continue;
                    }

                    if let Some(delim) = fence_delim(span.text) {
                        last_safe = span.start;
                        state = State::Fence(delim);
                    } else {
                        last_safe = span.end;
                        state = State::Outside;
                    }
                }
            }
        }

        last_safe
    }
}

impl Default for StreamController {
    fn default() -> Self {
        Self {
            collector: MarkdownStreamCollector::default(),
            stream_state: StreamState::default(),
            finalized: false,
            render_width: DEFAULT_STREAM_WIDTH,
            rendered_cache: Vec::new(),
        }
    }
}

enum State {
    Outside,
    Fence(FenceDelim),
    Table,
}

#[derive(Copy, Clone)]
enum FenceDelim {
    Backtick,
    Tilde,
}

impl FenceDelim {
    fn as_str(self) -> &'static str {
        match self {
            FenceDelim::Backtick => "```",
            FenceDelim::Tilde => "~~~",
        }
    }
}

struct LineSpan<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    has_newline: bool,
}

fn first_diverging_line(lhs: &[Line<'_>], rhs: &[Line<'_>]) -> usize {
    let mut i = 0;
    while i < lhs.len() && i < rhs.len() && lhs[i] == rhs[i] {
        i += 1;
    }
    i
}

fn line_spans(s: &str) -> Vec<LineSpan<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in s.as_bytes().iter().enumerate() {
        if *b == b'\n' {
            out.push(LineSpan {
                text: &s[start..i],
                start,
                end: i + 1,
                has_newline: true,
            });
            start = i + 1;
        }
    }

    if start < s.len() {
        out.push(LineSpan {
            text: &s[start..],
            start,
            end: s.len(),
            has_newline: false,
        });
    }
    out
}

fn fence_delim(line: &str) -> Option<FenceDelim> {
    let t = line.trim_start();
    if t.starts_with("```") {
        Some(FenceDelim::Backtick)
    } else if t.starts_with("~~~") {
        Some(FenceDelim::Tilde)
    } else {
        None
    }
}

fn is_fence_close(line: &str, delim: FenceDelim) -> bool {
    let t = line.trim_start();
    let prefix = delim.as_str();
    if !t.starts_with(prefix) {
        return false;
    }
    t[prefix.len()..].trim().is_empty()
}

fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Line;
    use std::time::Instant;
    use super::super::chunking::QueueSnapshot;

    fn lines_text(lines: &[Line<'_>]) -> String {
        let mut s = String::new();
        for line in lines {
            for span in &line.spans {
                s.push_str(&span.content);
            }
            s.push('\n');
        }
        s
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|span| span.content.to_string()).collect()
    }

    fn snapshot_body(c: &StreamController, width: u16) -> String {
        format!(
            "=== stable ===\n{}=== tail ===\n{}",
            lines_text(&c.stable_lines(width)),
            lines_text(&c.tail_lines(width)),
        )
    }

    #[test]
    fn partial_table_is_held_in_tail_until_finalize() {
        let mut c = StreamController::default();
        c.push_delta("Here is a table:\n");
        c.push_delta("| col a | col b |\n");
        c.push_delta("|-------|-------|\n");
        c.push_delta("| 1     | 2     |\n");

        let stable = lines_text(&c.stable_lines(60));
        assert!(
            !stable.contains('|'),
            "table content must not appear in stable while unterminated; got:\n{stable}"
        );
        let tail = lines_text(&c.tail_lines(60));
        assert!(
            tail.contains('|'),
            "table content must appear in tail while unterminated; got:\n{tail}"
        );

        c.finalize();
        let after = lines_text(&c.stable_lines(60));
        assert!(
            after.contains('|'),
            "table moves to stable once finalized; got:\n{after}"
        );
        let tail_after = lines_text(&c.tail_lines(60));
        assert!(
            tail_after.is_empty(),
            "tail must be empty after finalize; got:\n{tail_after}"
        );
    }

    #[test]
    fn unterminated_fence_keeps_body_in_tail() {
        let mut c = StreamController::default();
        c.push_delta("intro line\n```rust\nfn main() {\n");

        let stable = lines_text(&c.stable_lines(60));
        assert!(stable.contains("intro line"));
        assert!(!stable.contains("fn main"));

        let tail = lines_text(&c.tail_lines(60));
        assert!(tail.contains("fn main"));
    }

    #[test]
    fn closed_fence_moves_to_stable() {
        let mut c = StreamController::default();
        c.push_delta("```\nfn main() {}\n```\nafter\n");

        let stable = lines_text(&c.stable_lines(60));
        assert!(stable.contains("fn main"));
        assert!(stable.contains("after"));
    }

    #[test]
    fn snapshot_mid_stream_prose() {
        let mut c = StreamController::default();
        c.push_delta("Hello there.\nHow are you doi");

        insta::assert_snapshot!("mid_stream_prose", snapshot_body(&c, 40));
    }

    #[test]
    fn snapshot_mid_stream_table_held_back() {
        let mut c = StreamController::default();
        c.push_delta("Quick summary:\n");
        c.push_delta("| col a | col b |\n");
        c.push_delta("|-------|-------|\n");
        c.push_delta("| 1     | 2     |\n");

        insta::assert_snapshot!("mid_stream_table_held_back", snapshot_body(&c, 40));
    }

    #[test]
    fn snapshot_post_finalize() {
        let mut c = StreamController::default();
        c.push_delta("Quick summary:\n");
        c.push_delta("| col a | col b |\n");
        c.push_delta("|-------|-------|\n");
        c.push_delta("| 1     | 2     |\n");
        c.finalize();

        insta::assert_snapshot!("post_finalize", snapshot_body(&c, 40));
    }

    #[test]
    fn queue_drains_fifo() {
        let mut c = StreamController::default();
        c.push_delta("alpha\n");
        c.push_delta("beta\n");

        let first = c.on_commit_tick();
        let second = c.on_commit_tick();
        let third = c.on_commit_tick();

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert!(third.is_empty());
        assert_eq!(line_text(&first[0]), "  alpha");
        assert_eq!(line_text(&second[0]), "  beta");
        assert!(c.is_idle());
    }

    #[test]
    fn queue_snapshot_uses_oldest_age() {
        let mut c = StreamController::default();
        c.push_delta("alpha\n");

        let snapshot = QueueSnapshot {
            queued_lines: c.queued_lines(),
            oldest_age: c.oldest_queued_age(Instant::now()),
        };
        assert_eq!(snapshot.queued_lines, 1);
        assert!(snapshot.oldest_age.is_some());
    }
}
