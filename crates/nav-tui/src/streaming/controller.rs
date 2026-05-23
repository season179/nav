//! Stream-controller layer for assistant reply rendering.
//!
//! The controller keeps the raw streamed source, derives stable vs tail ranges
//! based on markdown fences/table rules, and pushes one tick unit into the
//! FIFO queue for each newly-stable *source line* (newline-terminated segment
//! in the model's output). The tail is rendered live — the user always sees
//! the partial line that's currently growing.
//!
//! ## Visibility gate
//!
//! `visible_stable_lines` counts source-text lines (not rendered lines)
//! released for display. [`visible_lines`] returns the rendered body for
//! `content` up to the *Nth* newline plus the live tail, where N is
//! `visible_stable_lines`. The commit-tick policy advances the counter one
//! line at a time in smooth mode and in bulk during catch-up, which is what
//! paces the perceived stream rate. Without the gate (or without anything
//! driving the tick) the entire stable region appears the moment the model
//! emits it, defeating the smoothing layer.
//!
//! Tracking by *source* lines (not rendered lines) makes the cap
//! width-independent: a resize re-wraps the rendered output but the source
//! line count is invariant, so visibility never drifts when the user
//! resizes mid-stream.

use std::time::{Duration, Instant};

use crate::cells::{count_wrapped_body_lines, render_body};
use ratatui::text::Line;

use super::StreamState;

#[derive(Default)]
pub(crate) struct StreamController {
    collector: MarkdownStreamCollector,
    stream_state: StreamState,
    finalized: bool,
    /// Highest byte offset in `collector.content()` we've already enqueued
    /// tick units for. Each delta extends this by counting newly-completed
    /// source lines in `content[enqueued_stable_offset..partition_offset]`
    /// and enqueueing one placeholder per newline.
    enqueued_stable_offset: usize,
    /// How many source-text lines in the stable region have been released
    /// for display. The visible body is the rendered body of content up to
    /// the Nth newline plus the live tail. Width-independent.
    visible_stable_lines: usize,
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
    /// Append `text` to the running stream source.
    pub(crate) fn push_delta(&mut self, text: &str) {
        if self.finalized {
            return;
        }

        self.collector.push(text);
        self.stream_state.has_seen_delta = true;
        self.requeue_stable();
    }

    /// Replace the streamed source with `text`, preserving finalize semantics.
    pub(crate) fn replace_buffer(&mut self, text: &str) {
        self.collector.replace(text);
        self.stream_state.clear_queue();
        self.stream_state.has_seen_delta = true;
        self.enqueued_stable_offset = 0;
        self.visible_stable_lines = 0;
        self.finalized = false;
        self.requeue_stable();
    }

    /// Mark stream complete and snap visibility to the entire body. After
    /// finalize the partition includes everything (no held-back tail), so we
    /// release every source line at once — pending commit ticks would
    /// otherwise leave the final reply half-revealed.
    ///
    /// `count_newlines + 1` covers a trailing partial line that has no
    /// newline of its own (replies that don't end with `\n`); the extra
    /// slot makes `nth_newline_offset` clamp to `content.len()` so the
    /// final segment renders.
    pub(crate) fn finalize(&mut self) {
        self.finalized = true;
        self.requeue_stable();
        self.visible_stable_lines =
            count_newlines(self.collector.content()).saturating_add(1);
        self.stream_state.clear_queue();
    }

    /// Drain one queued unit and release one more source line for display.
    /// Called by [`super::commit_tick::run_commit_tick`] under smooth mode.
    pub(crate) fn on_commit_tick(&mut self) -> Vec<Line<'static>> {
        let drained = self.stream_state.step();
        self.visible_stable_lines = self.visible_stable_lines.saturating_add(drained.len());
        drained
    }

    /// Drain up to `max_lines` queued units and release that many source
    /// lines for display. Used by catch-up mode when queue pressure builds.
    pub(crate) fn on_commit_tick_batch(&mut self, max_lines: usize) -> Vec<Line<'static>> {
        let drained = self.stream_state.drain_n(max_lines);
        self.visible_stable_lines = self.visible_stable_lines.saturating_add(drained.len());
        drained
    }

    /// Returns `true` when no lines are queued for commit.
    pub(crate) fn is_idle(&self) -> bool {
        self.stream_state.is_idle()
    }

    /// Number of queued tick units pending commit. Surfaces queue pressure
    /// to the chunking policy in units of source lines.
    pub(crate) fn queued_lines(&self) -> usize {
        self.stream_state.queued_len()
    }

    /// Age of the oldest queued tick unit. Surfaces queue staleness so a
    /// lone source line that has been waiting too long triggers catch-up
    /// even without depth pressure.
    pub(crate) fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.stream_state.oldest_queued_age(now)
    }

    /// Render the *entire* stable region (no visibility gate). Used by
    /// partition tests; the display path goes through [`visible_lines`].
    #[cfg(test)]
    pub(crate) fn stable_lines(&self, width: u16) -> Vec<Line<'static>> {
        let partition_end = self.partition_offset();
        render_body(&self.collector.content()[..partition_end], width)
    }

    #[cfg(test)]
    pub(crate) fn tail_lines(&self, width: u16) -> Vec<Line<'static>> {
        let partition_end = self.partition_offset();
        render_body(&self.collector.content()[partition_end..], width)
    }

    /// Visible body for display: the rendered prefix of stable content up
    /// to `visible_stable_lines` newlines, plus the live tail. The tail is
    /// always shown — only the stable region is gated, so partial input
    /// keeps flowing while the chunking policy paces the release of
    /// completed lines.
    pub(crate) fn visible_lines(&self, width: u16) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        let partition_end = self.partition_offset();
        let visible_end =
            nth_newline_offset(&self.collector.content()[..partition_end], self.visible_stable_lines);
        let stable = render_body(&self.collector.content()[..visible_end], width);
        let tail = render_body(&self.collector.content()[partition_end..], width);
        (stable, tail)
    }

    /// Visible body height without materializing `Vec<Line>`. The streaming
    /// cell calls this on the scroll hot path.
    pub(crate) fn visible_line_count(&self, width: u16) -> usize {
        let partition_end = self.partition_offset();
        let visible_end =
            nth_newline_offset(&self.collector.content()[..partition_end], self.visible_stable_lines);
        let stable = count_wrapped_body_lines(&self.collector.content()[..visible_end], width);
        let tail = count_wrapped_body_lines(&self.collector.content()[partition_end..], width);
        stable + tail
    }

    fn requeue_stable(&mut self) {
        if !self.stream_state.has_seen_delta {
            self.stream_state.clear_queue();
            self.enqueued_stable_offset = 0;
            return;
        }

        let partition_end = self.partition_offset();
        if self.enqueued_stable_offset >= partition_end {
            return;
        }

        // Each new newline in the stable region becomes one tick unit so
        // the chunking policy can pace per-source-line. We use placeholder
        // `Line::default()` entries because the display path renders fresh
        // from `collector.content()` — the queued Line values are never
        // read for paint, only counted and timed.
        let new_stable = &self.collector.content()[self.enqueued_stable_offset..partition_end];
        let new_newlines = count_newlines(new_stable);
        if new_newlines > 0 {
            let placeholders: Vec<Line<'static>> =
                std::iter::repeat_with(Line::default).take(new_newlines).collect();
            self.stream_state.enqueue(placeholders);
        }
        self.enqueued_stable_offset = partition_end;
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

/// Count `\n` bytes in `text`. Used for both queue-population sizing and
/// the final snap-to-total in `finalize`. UTF-8-safe because `\n` is a
/// single ASCII byte that can't appear inside a multi-byte sequence.
fn count_newlines(text: &str) -> usize {
    text.as_bytes().iter().filter(|b| **b == b'\n').count()
}

/// Byte offset *after* the `n`-th newline in `text`. Returns 0 for `n == 0`
/// and `text.len()` if fewer than `n` newlines exist. Used by the display
/// path to slice the stable region at the visibility boundary.
fn nth_newline_offset(text: &str, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let mut count = 0;
    for (i, b) in text.as_bytes().iter().enumerate() {
        if *b == b'\n' {
            count += 1;
            if count == n {
                return i + 1;
            }
        }
    }
    text.len()
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
    fn queue_drains_fifo_and_advances_visibility() {
        // Two source lines push two tick units onto the queue. Each
        // smooth-mode tick drains one and advances visible_stable_lines
        // by one; after both ticks the queue is idle. We check the
        // visible body (not the drained Lines themselves, which are
        // placeholders) — that's the contract the display path uses.
        let mut c = StreamController::default();
        c.push_delta("alpha\n");
        c.push_delta("beta\n");

        assert_eq!(c.queued_lines(), 2);

        let first = c.on_commit_tick();
        let visible_after_first = lines_text(&c.visible_lines(40).0);
        assert_eq!(first.len(), 1);
        assert!(
            visible_after_first.contains("alpha") && !visible_after_first.contains("beta"),
            "smooth tick 1 must reveal only the first source line; got:\n{visible_after_first}"
        );

        let second = c.on_commit_tick();
        let visible_after_second = lines_text(&c.visible_lines(40).0);
        assert_eq!(second.len(), 1);
        assert!(
            visible_after_second.contains("alpha") && visible_after_second.contains("beta"),
            "smooth tick 2 must reveal the second source line; got:\n{visible_after_second}"
        );

        let third = c.on_commit_tick();
        assert!(third.is_empty(), "no more queued units to drain");
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
