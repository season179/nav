//! Stream-controller layer for assistant reply rendering.
//!
//! The controller keeps the raw streamed source, derives stable vs tail ranges
//! based on markdown fences/table rules, and pushes one tick unit into the
//! FIFO queue for each newly-stable *source line* (newline-terminated segment
//! in the model's output). The tail is rendered live — the user always sees
//! the partial line that's currently growing.
//!
//! ## Current role in the migration
//!
//! `AssistantStreamingCell` owns one `StreamController` while the migration is
//! in progress. `ChatWidget` uses it to emit `AgentMessageCell` stable chunks
//! to scrollback while keeping the mutable tail as a `StreamingAgentTailCell`
//! in the viewport.
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

use crate::cells::{count_wrapped_body_lines, render_body, AgentMessageCell};
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
    /// How many source-text lines have been emitted as
    /// [`AgentMessageCell`] chunks via [`commit_tick_chunk`] /
    /// [`finalize_chunk`]. Lags behind `visible_stable_lines` whenever
    /// `requeue_stable` snaps over previously-visible tail content: the
    /// snap advances `visible_stable_lines` to keep the legacy display
    /// path stable, but the chunk-emission path still owes those lines
    /// to scrollback. Next emission covers the gap.
    emitted_stable_lines: usize,
    /// Set once the first stable chunk has been emitted via
    /// [`commit_tick_chunk`] (or [`finalize_chunk`]). Drives the
    /// `is_first_line` flag on emitted `AgentMessageCell`s and the
    /// `tail_starts_stream` hint used by the live tail cell.
    header_emitted: bool,
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

        let prev_content_len = self.collector.content().len();
        self.collector.push(text);
        self.stream_state.has_seen_delta = true;
        self.requeue_stable(prev_content_len);
    }

    /// Replace the streamed source with `text`, preserving finalize semantics.
    ///
    /// `header_emitted` and `emitted_stable_lines` are intentionally
    /// preserved: by the time a caller replaces the buffer (typically the
    /// resume-with-coalesced-text path mirrored from `into_finalized_with`)
    /// any chunks already pushed to scrollback can't be unsent. Resetting
    /// the header flag would let the chunk-emission path stamp a second
    /// `is_first_line=true` cell, putting two assistant bullets in the
    /// transcript for one logical reply. Resetting the emitted counter
    /// would re-emit content already in scrollback as a fresh chunk.
    pub(crate) fn replace_buffer(&mut self, text: &str) {
        self.collector.replace(text);
        self.stream_state.clear_queue();
        self.stream_state.has_seen_delta = true;
        self.enqueued_stable_offset = 0;
        self.visible_stable_lines = 0;
        self.finalized = false;
        // Treat the entire replaced buffer as freshly-arrived: no bytes were
        // previously visible to the user.
        self.requeue_stable(0);
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
        // Treat all content as "previously visible" so requeue_stable snaps
        // through it without enqueueing — finalize then sets vsl explicitly
        // and wipes the queue, so this is just bookkeeping for
        // `enqueued_stable_offset`.
        let len = self.collector.content().len();
        self.requeue_stable(len);
        self.visible_stable_lines = count_newlines(self.collector.content()).saturating_add(1);
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

    /// Smooth-mode tick that emits the newly-stable source range as an
    /// [`AgentMessageCell`] rendered at `width`. Mirrors Codex's
    /// `StreamController::on_commit_tick` contract: the cell is the stable
    /// chunk destined for scrollback, and the bool is `idle` (queue empty).
    ///
    /// Holdback and adaptive chunking semantics are unchanged — the chunk
    /// covers source between [`Self::emitted_stable_lines`] and the
    /// post-tick visibility boundary, so snap-on-advance jumps over
    /// previously-tailed content still flow into scrollback on the next
    /// tick instead of being lost.
    #[cfg_attr(not(test), allow(dead_code))] // wired by AM-04 (issue #222) ChatWidget rewire
    pub(crate) fn commit_tick_chunk(&mut self, width: u16) -> (Option<AgentMessageCell>, bool) {
        let before = self.unemitted_source_offset();
        let _ = self.on_commit_tick();
        let after = self.visible_source_offset();
        let cell = self.emit_chunk_between(before, after, width);
        self.emitted_stable_lines = self.visible_stable_lines;
        (cell, self.is_idle())
    }

    /// Catch-up variant of [`Self::commit_tick_chunk`] that drains up to
    /// `max_lines` queued tick units in a single batch. The emitted chunk
    /// covers the entire newly-released source range so the caller never
    /// has to stitch successive batch outputs together.
    #[cfg_attr(not(test), allow(dead_code))] // wired by AM-04 (issue #222) ChatWidget rewire
    pub(crate) fn commit_tick_batch_chunk(
        &mut self,
        width: u16,
        max_lines: usize,
    ) -> (Option<AgentMessageCell>, bool) {
        let before = self.unemitted_source_offset();
        let _ = self.on_commit_tick_batch(max_lines);
        let after = self.visible_source_offset();
        let cell = self.emit_chunk_between(before, after, width);
        self.emitted_stable_lines = self.visible_stable_lines;
        (cell, self.is_idle())
    }

    /// Render the mutable tail at `width` for the
    /// [`crate::cells::StreamingAgentTailCell`]. The tail is the source past
    /// the holdback partition (open fences, table rows, partial lines) —
    /// content that may still be reshaped by subsequent deltas.
    #[cfg_attr(not(test), allow(dead_code))] // wired by AM-04 (issue #222) ChatWidget rewire
    pub(crate) fn current_tail_lines(&self, width: u16) -> Vec<Line<'static>> {
        let partition_end = self.partition_offset();
        render_body(&self.collector.content()[partition_end..], width)
    }

    /// Whether the live tail should carry the assistant bullet prefix.
    /// False once a stable chunk has either emitted the header or is
    /// queued to become the header. This keeps AM-04's live tail from
    /// momentarily painting a second bullet while stable content is
    /// waiting for its commit tick.
    #[cfg_attr(not(test), allow(dead_code))] // wired by AM-04 (issue #222) ChatWidget rewire
    pub(crate) fn tail_starts_stream(&self) -> bool {
        !self.header_emitted && self.visible_stable_lines == 0 && self.stream_state.is_idle()
    }

    /// Finalize the stream and emit any source that hasn't been released
    /// yet (the entire remaining stable region plus the just-snapped tail)
    /// as a single [`AgentMessageCell`]. Returns the raw markdown source
    /// alongside the cell so the consolidation pass in AM-07 can replace
    /// the emitted chunk run with one source-backed [`crate::cells::AgentMarkdownCell`].
    #[cfg_attr(not(test), allow(dead_code))] // wired by AM-04 (issue #222) ChatWidget rewire
    pub(crate) fn finalize_chunk(&mut self, width: u16) -> (Option<AgentMessageCell>, String) {
        let before = self.unemitted_source_offset();
        let source = self.collector.content().to_string();
        self.finalize();
        let after = self.collector.content().len();
        let cell = self.emit_chunk_between(before, after, width);
        self.emitted_stable_lines = self.visible_stable_lines;
        (cell, source)
    }

    /// Byte offset of the next chunk's starting boundary — end of the Nth
    /// newline within the current stable partition, where N is
    /// [`Self::emitted_stable_lines`]. Lags behind
    /// [`Self::visible_source_offset`] whenever `requeue_stable` snaps
    /// over a previously-visible tail.
    fn unemitted_source_offset(&self) -> usize {
        let partition_end = self.partition_offset();
        nth_newline_offset(
            &self.collector.content()[..partition_end],
            self.emitted_stable_lines,
        )
    }

    /// Byte offset of the visibility boundary — end of the Nth newline
    /// within the current stable partition, where N is
    /// [`Self::visible_stable_lines`]. The boundary the legacy display
    /// path slices the rendered body at.
    fn visible_source_offset(&self) -> usize {
        let partition_end = self.partition_offset();
        nth_newline_offset(
            &self.collector.content()[..partition_end],
            self.visible_stable_lines,
        )
    }

    /// Render the source range `[start..end]` as an [`AgentMessageCell`]
    /// when non-empty. Drives the `header_emitted` flag forward so the
    /// next chunk renders without the assistant bullet.
    ///
    /// Both the prefix `[..start]` and the full `[..end]` are rendered so
    /// the chunk inherits the correct in-fence state at `start`. Rendering
    /// `[start..end]` in isolation would reset `render_body`'s local
    /// `in_fence` flag and re-style code-block bodies as prose whenever a
    /// fence's opener and body land on different commit ticks.
    fn emit_chunk_between(
        &mut self,
        start: usize,
        end: usize,
        width: u16,
    ) -> Option<AgentMessageCell> {
        if end <= start {
            return None;
        }
        let content = self.collector.content();
        let prefix_lines = render_body(&content[..start], width).len();
        let full = render_body(&content[..end], width);
        if full.len() <= prefix_lines {
            return None;
        }
        let lines = full[prefix_lines..].to_vec();
        let is_first = !self.header_emitted;
        self.header_emitted = true;
        Some(AgentMessageCell::new(lines, is_first))
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

    /// Raw markdown source accumulated for this assistant reply.
    pub(crate) fn source(&self) -> &str {
        self.collector.content()
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
        let visible_end = nth_newline_offset(
            &self.collector.content()[..partition_end],
            self.visible_stable_lines,
        );
        let stable = render_body(&self.collector.content()[..visible_end], width);
        let tail = render_body(&self.collector.content()[partition_end..], width);
        (stable, tail)
    }

    /// Visible body height without materializing `Vec<Line>`. The streaming
    /// cell calls this on the scroll hot path.
    pub(crate) fn visible_line_count(&self, width: u16) -> usize {
        let partition_end = self.partition_offset();
        let visible_end = nth_newline_offset(
            &self.collector.content()[..partition_end],
            self.visible_stable_lines,
        );
        let stable = count_wrapped_body_lines(&self.collector.content()[..visible_end], width);
        let tail = count_wrapped_body_lines(&self.collector.content()[partition_end..], width);
        stable + tail
    }

    /// Bring `enqueued_stable_offset` up to the current partition, splitting
    /// the newly-stable region into a "snap" portion (bytes that were
    /// already visible in tail before this delta) and a "gate" portion
    /// (bytes that arrived in this delta and are stable on arrival).
    ///
    /// `prev_content_len` is `collector.content().len()` *before* the
    /// current delta was appended. Anything in `[0..prev_content_len]` was
    /// on the user's screen the previous frame — either in the rendered
    /// stable prefix or in the live tail. When partition_offset advances
    /// over that range we must keep showing it; otherwise the user
    /// perceives a flash as content briefly disappears and re-emerges over
    /// the next few commit ticks.
    fn requeue_stable(&mut self, prev_content_len: usize) {
        if !self.stream_state.has_seen_delta {
            self.stream_state.clear_queue();
            self.enqueued_stable_offset = 0;
            return;
        }

        let partition_end = self.partition_offset();
        if self.enqueued_stable_offset >= partition_end {
            return;
        }

        let content = self.collector.content();

        // Compute the snap boundary: the highest byte position such that
        // everything before it was visible in the previous frame.
        //
        // - `snap_target = min(prev_content_len, partition_end)` is the
        //   bytes that newly entered the stable region but were *also* part
        //   of the previously-rendered content (either stable prefix or
        //   live tail).
        //
        // - If `snap_target` lands mid-line (the byte just before it is
        //   not a newline), the partial line that was in tail extends past
        //   `snap_target`. Snap forward through the next `\n` so the line
        //   the user was already reading stays on screen unbroken.
        let snap_target = prev_content_len.min(partition_end);
        let snap_byte_boundary = compute_snap_boundary(content, snap_target, partition_end);

        if snap_byte_boundary > self.enqueued_stable_offset {
            let target_vsl = count_newlines(&content[..snap_byte_boundary]);
            // Only ever move visibility forward — never reveal less than
            // the user already saw.
            self.visible_stable_lines = self.visible_stable_lines.max(target_vsl);
        }

        // Bytes past the snap boundary are content that arrived in this
        // delta and entered the stable region without ever being shown in
        // tail. Gate them through the chunking policy so the user sees a
        // smooth reveal instead of a wall of text.
        //
        // Placeholder `Line::default()` entries are used because the
        // display path renders fresh from `collector.content()` — queued
        // Lines are never read for paint, only counted and timed.
        let queue_start = snap_byte_boundary.max(self.enqueued_stable_offset);
        if queue_start < partition_end {
            let new_newlines = count_newlines(&content[queue_start..partition_end]);
            if new_newlines > 0 {
                let placeholders: Vec<Line<'static>> = std::iter::repeat_with(Line::default)
                    .take(new_newlines)
                    .collect();
                self.stream_state.enqueue(placeholders);
            }
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

/// Compute the snap boundary for `requeue_stable`. Returns the smallest byte
/// position `b` in `[snap_target..=partition_end]` such that `content[..b]`
/// covers all source lines that were visible to the user in the previous
/// frame.
///
/// When `snap_target` is right after a newline (or is `0`), the previous
/// line ended exactly there and no extension is needed. When it lands
/// mid-line, the partial line that was already rendered in tail continues
/// past `snap_target`; the boundary extends through the next `\n` (or to
/// `partition_end` if no newline exists in the remainder) so the line the
/// user was reading stays visible after the partition advances.
fn compute_snap_boundary(content: &str, snap_target: usize, partition_end: usize) -> usize {
    if snap_target == 0 {
        return 0;
    }
    let bytes = content.as_bytes();
    if bytes[snap_target - 1] == b'\n' {
        return snap_target;
    }
    let mut i = snap_target;
    while i < partition_end && bytes[i] != b'\n' {
        i += 1;
    }
    if i < partition_end {
        i + 1
    } else {
        partition_end
    }
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
    use super::super::chunking::QueueSnapshot;
    use super::*;
    use crate::history::HistoryCell;
    use ratatui::text::Line;
    use std::time::Instant;

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
    fn newline_completing_visible_tail_does_not_shrink_display() {
        // The flash regression scenario: stream "hello world" — visible
        // in the live tail — then push "\n". The newline reclassifies
        // "hello world" from tail to stable. Without the snap-on-advance
        // fix, visible_stable_lines stays at 0 and the row briefly
        // disappears until the next commit tick. With the fix, the row
        // remains visible across the delta.
        let mut c = StreamController::default();
        c.push_delta("hello world");

        let before = lines_text(&c.visible_lines(40).1);
        assert!(
            before.contains("hello world"),
            "partial line must render in tail; got tail:\n{before}"
        );

        c.push_delta("\n");

        let (stable_after, tail_after) = c.visible_lines(40);
        let combined = format!("{}{}", lines_text(&stable_after), lines_text(&tail_after));
        assert!(
            combined.contains("hello world"),
            "completing newline must not erase the previously-visible line; got:\n{combined}"
        );
    }

    #[test]
    fn fence_close_does_not_shrink_previously_visible_code_block() {
        // Stream an opening fence and a body line — those rows render in
        // the live tail. Then push the closing fence: partition_offset
        // jumps past the entire fenced region. Pre-fix, every code line
        // briefly disappears and replays one-per-tick. Post-fix, the
        // already-visible code stays on screen.
        let mut c = StreamController::default();
        c.push_delta("intro\n");
        // Drive a smooth tick so "intro" is also released for display —
        // mirrors the real frame loop where ticks run between deltas.
        let _ = c.on_commit_tick();

        c.push_delta("```rust\n");
        c.push_delta("fn main() {\n");

        let (stable_pre, tail_pre) = c.visible_lines(80);
        let pre_close = format!("{}{}", lines_text(&stable_pre), lines_text(&tail_pre));
        assert!(pre_close.contains("```rust") && pre_close.contains("fn main() {"));

        c.push_delta("}\n```\n");

        let (stable_post, tail_post) = c.visible_lines(80);
        let post_close = format!("{}{}", lines_text(&stable_post), lines_text(&tail_post));
        assert!(
            post_close.contains("intro")
                && post_close.contains("```rust")
                && post_close.contains("fn main() {"),
            "fence-close must keep previously-visible code rows on screen; got:\n{post_close}"
        );
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

    #[test]
    fn replace_buffer_preserves_header_state_after_chunk_emission() {
        // Once a stable chunk has been pushed to scrollback, the
        // assistant bullet is already committed to the user's terminal.
        // A subsequent replace_buffer (e.g. the resume-with-coalesced-
        // text path mirrored from `into_finalized_with`) must NOT reset
        // header_emitted, or the follow-up finalize_chunk will emit a
        // second is_first_line=true cell and the user sees two
        // assistant bullets for one logical reply.
        let mut c = StreamController::default();
        c.push_delta("Hello\n");
        let first = c.commit_tick_chunk(40).0.expect("first chunk should emit");
        assert!(first.is_first_line());
        assert!(!c.tail_starts_stream());

        c.replace_buffer("Hello world\n");
        assert!(
            !c.tail_starts_stream(),
            "replace_buffer must NOT reset header_emitted once chunks are in scrollback"
        );

        if let (Some(cell), _) = c.finalize_chunk(40) {
            assert!(
                !cell.is_first_line(),
                "finalize_chunk after replace_buffer must mark continuation, not a fresh bullet"
            );
        }
    }

    #[test]
    fn closed_fence_drained_per_tick_keeps_code_styling_inside_body() {
        // render_body tracks `in_fence` locally per call. Slicing the
        // source per smooth tick and rendering each slice independently
        // resets in_fence between chunks, so code-block body lines fall
        // through to body_line + parse_inline_spans and lose cyan
        // styling (worse, literal `*` / `_` get interpreted as markdown).
        // emit_chunk_between must render with prior fence context so
        // body chunks render as code regardless of how lines drain.
        use ratatui::style::Color;

        let mut c = StreamController::default();
        c.push_delta("```\nfn *bold* main() {}\n```\nafter\n");

        let mut all_lines: Vec<Line<'static>> = Vec::new();
        for _ in 0..6 {
            if let Some(cell) = c.commit_tick_chunk(40).0 {
                all_lines.extend(cell.display_lines(40));
            }
        }

        fn line_text(line: &Line<'_>) -> String {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        }

        let body_line = all_lines
            .iter()
            .find(|line| {
                let text = line_text(line);
                text.contains("fn") && text.contains("main")
            })
            .unwrap_or_else(|| {
                let rendered: Vec<String> = all_lines.iter().map(line_text).collect();
                panic!(
                    "rendered output should include the code-block body line; got: {rendered:?}"
                );
            });
        let has_cyan_span = body_line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(Color::Cyan));
        assert!(
            has_cyan_span,
            "fenced-code body must stay cyan when drained per-tick; got line spans: {:?}",
            body_line.spans
        );
        let combined = line_text(body_line);
        assert!(
            combined.contains("*bold*"),
            "code-block body must preserve literal markdown markers (no inline parsing); got: {combined:?}"
        );
    }

    #[test]
    fn open_fence_body_stays_in_tail_until_close_or_finalize() {
        // The holdback rule must survive the chunk-emission rewrite: an
        // unterminated fenced block keeps its body in `current_tail_lines`
        // and out of every emitted AgentMessageCell. Once the fence
        // closes, the previously-tailed lines move into the stable
        // partition and are released by commit ticks normally.
        let mut c = StreamController::default();
        c.push_delta("intro\n```rust\nfn main() {\n");

        let first = c.commit_tick_chunk(80).0.expect("intro line should emit");
        let first_rendered = lines_text(&first.display_lines(80));
        assert!(
            first_rendered.contains("intro"),
            "intro line is stable and must emit; got:\n{first_rendered}"
        );
        assert!(
            !first_rendered.contains("```rust") && !first_rendered.contains("fn main"),
            "open fence must hold its body back from the stable chunk; got:\n{first_rendered}"
        );

        let tail = lines_text(&c.current_tail_lines(80));
        assert!(
            tail.contains("```rust") && tail.contains("fn main"),
            "fenced body must render via current_tail_lines while the fence is open; got:\n{tail}"
        );

        c.push_delta("}\n```\n");

        let mut combined = String::new();
        while let (Some(chunk), idle) = c.commit_tick_batch_chunk(80, 16) {
            combined.push_str(&lines_text(&chunk.display_lines(80)));
            if idle {
                break;
            }
        }
        let (final_cell, source) = c.finalize_chunk(80);
        if let Some(cell) = final_cell {
            combined.push_str(&lines_text(&cell.display_lines(80)));
        }
        assert!(
            combined.contains("```rust") && combined.contains("fn main") && combined.contains('}'),
            "after fence close + finalize, the body must land in emitted chunks; got:\n{combined}"
        );
        assert_eq!(
            source, "intro\n```rust\nfn main() {\n}\n```\n",
            "raw source must contain the full markdown for consolidation"
        );
    }

    #[test]
    fn commit_tick_batch_chunk_emits_all_drained_lines_in_order() {
        // Catch-up mode (depth >= 8 or stale oldest entry) drains many
        // queued ticks in one call. The emitted chunk must contain every
        // newly-released source line, preserving their original order, so
        // bursty model output doesn't show up out of sequence.
        let mut c = StreamController::default();
        for i in 0..10 {
            c.push_delta(&format!("line {i}\n"));
        }
        assert_eq!(c.queued_lines(), 10);

        let (cell, idle) = c.commit_tick_batch_chunk(60, 10);
        let cell = cell.expect("batch tick should emit a chunk");
        assert!(
            idle,
            "10 queued lines drained in one batch leaves queue empty"
        );
        let rendered = lines_text(&cell.display_lines(60));
        for i in 0..10 {
            assert!(
                rendered.contains(&format!("line {i}")),
                "batch chunk missing line {i}; got:\n{rendered}"
            );
        }
        let pos_line0 = rendered.find("line 0").unwrap();
        let pos_line9 = rendered.find("line 9").unwrap();
        assert!(
            pos_line0 < pos_line9,
            "batch must preserve original source order; got:\n{rendered}"
        );
    }

    #[test]
    fn finalize_chunk_emits_remaining_and_returns_raw_source() {
        // After streaming partial content with held-back lines, finalize
        // must drain everything left as a single AgentMessageCell *and*
        // hand back the full markdown source. The source feeds the
        // consolidation pass (AM-07) that replaces the emitted chunk run
        // with one AgentMarkdownCell, so losing it would break resize
        // reflow on finalized scrollback.
        let mut c = StreamController::default();
        c.push_delta("intro\n");
        c.push_delta("```rust\nfn main() {}\n");

        let (cell, source) = c.finalize_chunk(60);
        let cell = cell.expect("finalize must emit remaining source as a chunk");
        assert_eq!(
            source, "intro\n```rust\nfn main() {}\n",
            "raw markdown source must be preserved verbatim for consolidation"
        );
        let rendered = lines_text(&cell.display_lines(60));
        assert!(
            rendered.contains("intro")
                && rendered.contains("```rust")
                && rendered.contains("fn main"),
            "remaining stable + held-back tail must appear in finalize chunk; got:\n{rendered}"
        );
        assert!(
            cell.is_first_line(),
            "finalize without prior chunks owns the assistant bullet"
        );
    }

    #[test]
    fn finalize_chunk_after_prior_chunks_marks_continuation() {
        // When commit_tick_chunk has already emitted the assistant
        // header, finalize must NOT re-set is_first_line; the bullet
        // belongs to the very first chunk only.
        let mut c = StreamController::default();
        c.push_delta("alpha\n");
        c.push_delta("beta\n");

        let first = c.commit_tick_chunk(40).0.expect("first chunk should emit");
        assert!(first.is_first_line());

        let (final_cell, _) = c.finalize_chunk(40);
        let final_cell = final_cell.expect("finalize should emit the remaining beta line");
        assert!(
            !final_cell.is_first_line(),
            "finalize after a chunk run is a continuation, not a header"
        );
    }

    #[test]
    fn tail_starts_stream_is_false_while_header_chunk_is_queued() {
        // Mirrors Codex's contract: once stable content is queued, that
        // stable chunk owns the assistant bullet even before the commit
        // tick moves it into scrollback. The mutable live tail should
        // already render as a continuation so AM-04 never shows two
        // bullets for one assistant response.
        let mut c = StreamController::default();
        c.push_delta("alpha\nbeta");

        assert!(
            !c.tail_starts_stream(),
            "queued stable content will own the stream header"
        );
        let tail = lines_text(&c.current_tail_lines(40));
        assert!(
            tail.contains("beta"),
            "partial tail remains visible; got:\n{tail}"
        );

        let first = c
            .commit_tick_chunk(40)
            .0
            .expect("queued header chunk should emit");
        assert!(first.is_first_line());
        assert!(!c.tail_starts_stream());
    }

    #[test]
    fn tail_starts_stream_flips_after_first_chunk() {
        // ChatWidget reads this hint to decide whether the next
        // StreamingAgentTailCell should carry the assistant bullet. Once
        // the controller has emitted any chunk (or finalize cell), the
        // tail is a continuation of the message and must keep the body
        // indent.
        let mut c = StreamController::default();
        c.push_delta("alpha");
        assert!(
            c.tail_starts_stream(),
            "no chunk emitted yet — next tail starts the stream"
        );

        c.push_delta("\n");
        let _ = c.commit_tick_chunk(40);
        assert!(
            !c.tail_starts_stream(),
            "first chunk consumed the stream header"
        );
    }

    #[test]
    fn current_tail_lines_exposes_only_mutable_tail() {
        // Push a stable line plus content that the holdback rule keeps in
        // the live tail (an unterminated fence). `current_tail_lines`
        // must render the tail at the requested width and skip the
        // already-stable prose so the StreamingAgentTailCell never
        // double-paints scrollback content.
        let mut c = StreamController::default();
        c.push_delta("intro line\n");
        c.push_delta("```rust\nfn main() {\n");

        let tail = lines_text(&c.current_tail_lines(60));
        assert!(
            tail.contains("```rust") && tail.contains("fn main"),
            "live tail must include the unterminated fence body; got:\n{tail}"
        );
        assert!(
            !tail.contains("intro line"),
            "stable content must not leak into the tail; got:\n{tail}"
        );
    }

    #[test]
    fn commit_tick_chunk_marks_first_chunk_and_continuations_separately() {
        // The first emitted chunk in a stream carries the assistant
        // bullet; later chunks must keep the body indent so the rendered
        // transcript reads as one continuous assistant message. The
        // controller owns this state — callers should never have to
        // track "is this the first chunk?" themselves.
        let mut c = StreamController::default();
        c.push_delta("alpha\n");
        c.push_delta("beta\n");

        let first = c
            .commit_tick_chunk(40)
            .0
            .expect("first smooth tick should emit a chunk");
        assert!(first.is_first_line(), "first chunk must mark is_first_line");

        let second = c
            .commit_tick_chunk(40)
            .0
            .expect("second smooth tick should emit a chunk");
        assert!(
            !second.is_first_line(),
            "subsequent chunk must not re-mark is_first_line"
        );
    }

    #[test]
    fn commit_tick_chunk_emits_newly_stable_line_only() {
        // Two source lines stable → one queued tick unit per line. A
        // single smooth-mode commit tick must dequeue only the first line
        // and emit it as an AgentMessageCell; the second line stays
        // queued. This is the contract the AM-04 ChatWidget wiring relies
        // on: each tick produces one stable chunk for scrollback.
        let mut c = StreamController::default();
        c.push_delta("alpha\n");
        c.push_delta("beta\n");

        let (cell, idle) = c.commit_tick_chunk(40);
        let cell = cell.expect("smooth tick should emit one chunk");
        assert!(!idle, "second source line is still queued");
        let rendered = lines_text(&cell.display_lines(40));
        assert!(
            rendered.contains("alpha") && !rendered.contains("beta"),
            "smooth tick must release only the first source line; got:\n{rendered}"
        );
    }

    #[test]
    fn commit_tick_chunk_emits_snapped_tail_once() {
        // Nav keeps partial lines visible in the live tail. When the
        // newline arrives, `requeue_stable` snaps that already-visible
        // tail into the stable partition without queueing a drain unit.
        // The chunk-emission path must still produce one stable
        // AgentMessageCell for scrollback, then remember that it has
        // emitted the snapped line.
        let mut c = StreamController::default();
        c.push_delta("hello world");
        assert!(!c.current_tail_lines(40).is_empty());

        c.push_delta("\n");
        assert_eq!(
            c.queued_lines(),
            0,
            "snapped tail was already visible, so it should not be queued"
        );

        let (cell, idle) = c.commit_tick_chunk(40);
        assert!(idle);
        let cell = cell.expect("snapped stable tail still needs a scrollback chunk");
        let rendered = lines_text(&cell.display_lines(40));
        assert!(
            rendered.contains("hello world"),
            "snapped tail must emit as a stable chunk; got:\n{rendered}"
        );

        let (duplicate, duplicate_idle) = c.commit_tick_chunk(40);
        assert!(duplicate_idle);
        assert!(
            duplicate.is_none(),
            "snapped tail must not emit twice after the emitted counter advances"
        );
    }
}
