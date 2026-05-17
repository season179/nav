//! Streaming partition for live assistant output.
//!
//! Partition rule: lines that end with a hard newline AND aren't inside an
//! unterminated markdown block (fenced code, table) move to stable; the rest
//! stay in tail and re-render each delta. [`StreamController::finalize`]
//! flushes whatever is still in tail into stable.

use ratatui::text::Line;

use crate::cells::render_body;

#[derive(Default)]
pub struct StreamController {
    buffer: String,
    finalized: bool,
}

impl StreamController {
    pub fn push_delta(&mut self, text: &str) {
        if self.finalized {
            return;
        }
        self.buffer.push_str(text);
    }

    /// Mark the stream as complete. After this call the whole buffer is
    /// treated as stable, regardless of any open markdown block.
    pub fn finalize(&mut self) {
        self.finalized = true;
    }

    pub fn stable_lines(&self, width: u16) -> Vec<Line<'static>> {
        let end = self.partition_offset();
        render_body(&self.buffer[..end], width)
    }

    pub fn tail_lines(&self, width: u16) -> Vec<Line<'static>> {
        let end = self.partition_offset();
        render_body(&self.buffer[end..], width)
    }

    /// Render both halves in a single buffer scan — used on the render hot
    /// path so callers don't pay for two `partition_offset` walks per frame.
    pub fn partitioned_lines(&self, width: u16) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
        let end = self.partition_offset();
        (
            render_body(&self.buffer[..end], width),
            render_body(&self.buffer[end..], width),
        )
    }

    fn partition_offset(&self) -> usize {
        if self.finalized {
            return self.buffer.len();
        }

        let mut state = State::Outside;
        let mut last_safe: usize = 0;

        for span in line_spans(&self.buffer) {
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
                    // First non-table-row terminates the table; commit the
                    // table's bytes, then handle this line in `Outside`.
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
    let d = delim.as_str();
    if !t.starts_with(d) {
        return false;
    }
    t[d.len()..].trim().is_empty()
}

fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
