//! Bounds tool output before it reaches the model prompt or session log.
//!
//! Without a cap, a single `bash` or `read_file` can blow up the context
//! window and inflate every subsequent turn (including the persisted SQLite
//! event log). This module applies a dual cap (`MAX_LINES` *or* `MAX_BYTES`,
//! whichever is hit first) and inserts a single human-readable marker noting
//! what was dropped. Bounding happens once at the tool boundary so the same
//! truncated `String` flows into both the prompt and the session log.
//!
//! Per-tool sub-caps live alongside this global cap: `code_search` clips
//! individual match lines to `GREP_MAX_LINE_LENGTH` and caps to 100 matches;
//! `list_files` caps directory entries to 500. The per-tool caps fire first
//! at the source so the global cap below is rarely the binding limit. See
//! `docs/per-turn-token-bounding-prd.md` for the rationale.

pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;
/// Per-line clip for grep matches. Long minified-file hits or generated-code
/// lines can swamp the model context without adding signal; clipping each
/// match line keeps the surrounding matches readable.
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Stricter line cap applied to `read_file` output. File reads are the most
/// common way to flood context with low-signal bytes (large generated files,
/// vendored code, lockfiles), so the per-tool cap is a quarter of the
/// generic `MAX_LINES`. The byte cap stays at `MAX_BYTES`.
pub const READ_FILE_MAX_LINES: usize = 500;
pub const READ_FILE_MAX_BYTES: usize = MAX_BYTES;

/// Result of a bound: the truncated content plus whether anything was
/// dropped. Callers use the flag to attach metadata to the durable tool
/// output event so replay/UI can show why a tool result is short.
///
/// `kept_full_lines` counts *complete* lines retained by the bound. A line
/// retained as a byte-bounded prefix (long minified content) does not count.
/// Callers that need to compute resume offsets after a byte truncation use
/// this to rebuild a correct "next offset" hint — relying on the pre-bound
/// trailer would otherwise overstate progress and silently skip dropped lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedOutput {
    pub content: String,
    pub truncated: bool,
    pub kept_full_lines: usize,
}

impl BoundedOutput {
    /// Render the bound's truncation as the shared
    /// [`crate::tool_registry::TruncationMeta`] used by dispatch and durable
    /// events. Returns `None` when nothing was dropped so the caller can
    /// pass the result straight into `ToolResult::truncation`.
    pub fn truncation_meta(
        &self,
        cause: crate::tool_registry::TruncationKind,
    ) -> Option<crate::tool_registry::TruncationMeta> {
        self.truncated.then_some(crate::tool_registry::TruncationMeta {
            truncated_by: cause,
            full_output_path: None,
            artifact_id: None,
        })
    }
}

/// Truncation strategy chosen per tool.
///
/// `Head` keeps the prefix — used for `read_file` and `code_search`, where
/// the earliest matches are usually the most useful. `HeadTail` keeps a
/// short prefix and the remaining budget at the tail — used for `bash`,
/// where shell errors tend to appear at the end of the output.
#[derive(Debug, Clone, Copy)]
pub enum TruncateMode {
    Head,
    HeadTail { head_lines: usize },
}

/// Bound `output` using the default `MAX_LINES` / `MAX_BYTES` limits.
pub fn bound(output: String, mode: TruncateMode) -> BoundedOutput {
    bound_with_limits(output, mode, MAX_LINES, MAX_BYTES)
}

pub fn bound_with_limits(
    output: String,
    mode: TruncateMode,
    max_lines: usize,
    max_bytes: usize,
) -> BoundedOutput {
    let total_bytes = output.len();
    let lines: Vec<&str> = output.split_inclusive('\n').collect();
    let total_lines = lines.len();
    if total_bytes <= max_bytes && total_lines <= max_lines {
        return BoundedOutput {
            content: output,
            truncated: false,
            kept_full_lines: total_lines,
        };
    }

    let (content, kept_full_lines) = match mode {
        TruncateMode::Head => render_head(&lines, max_lines, max_bytes, total_lines, total_bytes),
        TruncateMode::HeadTail { head_lines } => render_head_tail(
            &lines,
            head_lines,
            max_lines,
            max_bytes,
            total_lines,
            total_bytes,
        ),
    };
    BoundedOutput {
        content,
        truncated: true,
        kept_full_lines,
    }
}

fn marker(dropped_bytes: usize, dropped_lines: usize) -> String {
    format!("\n[truncated {dropped_bytes} bytes / {dropped_lines} lines]\n")
}

/// Clip a single line to fit `max_bytes`, appending `... [truncated]` when
/// the line is over budget. The byte budget is enforced at a UTF-8
/// character boundary so the returned text is always valid UTF-8.
///
/// Returns `(text, was_truncated)`. Mirrors pi's `truncateLine` shape so we
/// can keep the grep-line clip behavior in sync.
pub fn truncate_line(line: &str, max_bytes: usize) -> (String, bool) {
    if line.len() <= max_bytes {
        return (line.to_string(), false);
    }
    let prefix = byte_prefix(line, max_bytes);
    let suffix = "... [truncated]";
    let mut out = String::with_capacity(prefix.len() + suffix.len());
    out.push_str(prefix);
    out.push_str(suffix);
    (out, true)
}

/// Largest UTF-8-safe prefix of `s` not exceeding `max` bytes. Walks back to
/// the nearest char boundary so the returned slice is always valid UTF-8.
pub(crate) fn byte_prefix(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Largest UTF-8-safe suffix of `s` not exceeding `max` bytes.
fn byte_suffix(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

fn render_head(
    lines: &[&str],
    max_lines: usize,
    max_bytes: usize,
    total_lines: usize,
    total_bytes: usize,
) -> (String, usize) {
    let mut kept_text = String::new();
    let mut kept_lines = 0usize;
    let mut kept_bytes = 0usize;
    for line in lines {
        if kept_lines >= max_lines {
            break;
        }
        let remaining = max_bytes - kept_bytes;
        if line.len() > remaining {
            // Long single line (minified JSON/JS, ripgrep hit on a generated
            // file). Don't drop the whole line — keep a byte-bounded prefix
            // at the nearest UTF-8 boundary so the model sees real content.
            // This partial line is NOT counted in `kept_lines` — callers
            // computing a resume offset must point at this line again to
            // get the rest of it.
            let prefix = byte_prefix(line, remaining);
            kept_text.push_str(prefix);
            kept_bytes += prefix.len();
            break;
        }
        kept_text.push_str(line);
        kept_bytes += line.len();
        kept_lines += 1;
    }

    let dropped_lines = total_lines.saturating_sub(kept_lines);
    let dropped_bytes = total_bytes.saturating_sub(kept_bytes);
    let mut result = String::with_capacity(kept_bytes + 64);
    result.push_str(&kept_text);
    result.push_str(&marker(dropped_bytes, dropped_lines));
    (result, kept_lines)
}

fn render_head_tail(
    lines: &[&str],
    head_lines_budget: usize,
    max_lines: usize,
    max_bytes: usize,
    total_lines: usize,
    total_bytes: usize,
) -> (String, usize) {
    let max_lines = max_lines.max(1);
    let head_lines_budget = head_lines_budget.min(max_lines);
    let tail_lines_budget = max_lines - head_lines_budget;

    // Split the byte budget proportionally to the line budget so a 200/1800
    // line split also gives roughly 200/1800 of the bytes to each piece.
    let head_byte_budget = max_bytes
        .saturating_mul(head_lines_budget)
        .checked_div(max_lines)
        .unwrap_or(0);
    let tail_byte_budget = max_bytes.saturating_sub(head_byte_budget);

    let mut head_text = String::new();
    let mut head_lines_kept = 0usize;
    let mut head_bytes_kept = 0usize;
    for line in lines {
        if head_lines_kept >= head_lines_budget {
            break;
        }
        let remaining = head_byte_budget - head_bytes_kept;
        if line.len() > remaining {
            let prefix = byte_prefix(line, remaining);
            head_text.push_str(prefix);
            head_bytes_kept += prefix.len();
            break;
        }
        head_text.push_str(line);
        head_bytes_kept += line.len();
        head_lines_kept += 1;
    }

    // The tail starts after the lines the head already consumed, so a
    // small input never gets the same line in both segments.
    let remaining_lines = &lines[head_lines_kept..];
    let mut tail_chunks: Vec<&str> = Vec::new();
    let mut tail_lines_kept = 0usize;
    let mut tail_bytes_kept = 0usize;
    for line in remaining_lines.iter().rev() {
        if tail_lines_kept >= tail_lines_budget {
            break;
        }
        let remaining = tail_byte_budget - tail_bytes_kept;
        if line.len() > remaining {
            // Single line overflows the tail budget — keep its byte-bounded
            // suffix (closer to whole tail lines we already collected).
            let suffix = byte_suffix(line, remaining);
            tail_chunks.push(suffix);
            tail_bytes_kept += suffix.len();
            break;
        }
        tail_chunks.push(line);
        tail_bytes_kept += line.len();
        tail_lines_kept += 1;
    }
    let mut tail_text = String::new();
    for line in tail_chunks.iter().rev() {
        tail_text.push_str(line);
    }

    let kept_lines = head_lines_kept + tail_lines_kept;
    let kept_bytes = head_bytes_kept + tail_bytes_kept;
    let dropped_lines = total_lines.saturating_sub(kept_lines);
    let dropped_bytes = total_bytes.saturating_sub(kept_bytes);

    let mut result = String::with_capacity(kept_bytes + 64);
    result.push_str(&head_text);
    result.push_str(&marker(dropped_bytes, dropped_lines));
    result.push_str(&tail_text);
    (result, kept_lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_passes_through_unchanged() {
        let input = "hello\nworld\n".to_string();
        let result = bound(input.clone(), TruncateMode::Head);
        assert_eq!(result.content, input);
        assert!(!result.truncated);
    }

    #[test]
    fn head_caps_at_max_lines() {
        let mut input = String::new();
        for i in 0..50 {
            input.push_str(&format!("line {i}\n"));
        }
        let result = bound_with_limits(input.clone(), TruncateMode::Head, 10, 1024);
        assert!(result.truncated);
        assert!(result.content.starts_with("line 0\n"));
        assert!(result.content.contains("[truncated"));
        assert!(result.content.contains("40 lines"));
        // None of the dropped lines should appear.
        assert!(!result.content.contains("line 10\n"));
    }

    #[test]
    fn head_caps_at_max_bytes() {
        let line = "x".repeat(100) + "\n";
        let input = line.repeat(50); // 5050 bytes
        let result = bound_with_limits(input, TruncateMode::Head, 1000, 500);
        assert!(result.truncated);
        assert!(result.content.len() < 1024); // 500 cap + marker
        assert!(result.content.contains("[truncated"));
        assert!(result.content.contains("bytes"));
    }

    #[test]
    fn head_marker_shows_dropped_counts() {
        let mut input = String::new();
        for _ in 0..30 {
            input.push_str("abc\n"); // 4 bytes each = 120 total
        }
        let result = bound_with_limits(input, TruncateMode::Head, 5, 1024);
        // 5 kept, 25 dropped; 5*4=20 kept, 100 dropped.
        assert!(result.truncated);
        assert!(result.content.contains("[truncated 100 bytes / 25 lines]"));
    }

    #[test]
    fn head_tail_keeps_first_and_last_lines() {
        let mut input = String::new();
        for i in 0..100 {
            input.push_str(&format!("{i}\n"));
        }
        let result = bound_with_limits(input, TruncateMode::HeadTail { head_lines: 3 }, 10, 10_000);
        // Head: 0,1,2
        assert!(result.truncated);
        assert!(result.content.starts_with("0\n1\n2\n"));
        // Tail: 93..99 (10 - 3 = 7 tail lines).
        assert!(result.content.contains("\n99\n"));
        assert!(result.content.contains("\n93\n"));
        // Middle should be gone.
        assert!(!result.content.contains("\n50\n"));
        assert!(result.content.contains("[truncated"));
    }

    #[test]
    fn head_tail_handles_byte_cap_before_line_cap() {
        // Build wide lines so byte budget hits before line budget.
        let line = "y".repeat(80) + "\n"; // 81 bytes
        let input = line.repeat(100); // 8100 bytes
        let result = bound_with_limits(input, TruncateMode::HeadTail { head_lines: 2 }, 50, 500);
        assert!(result.truncated);
        assert!(result.content.len() <= 600); // cap + marker headroom
        assert!(result.content.contains("[truncated"));
    }

    #[test]
    fn head_tail_does_not_overlap_head_and_tail_lines() {
        // 8 lines, asking for head=3 + tail=4 (max_lines=7). Line 3 must not
        // appear twice and line 7 must be present.
        let input = (0..8).map(|i| format!("line{i}\n")).collect::<String>();
        let result = bound_with_limits(input, TruncateMode::HeadTail { head_lines: 3 }, 7, 10_000);
        let occurrences = result.content.matches("line3\n").count();
        assert!(
            occurrences <= 1,
            "line3 appeared {occurrences} times: {}",
            result.content
        );
        assert!(result.content.contains("line0\n"));
        assert!(result.content.contains("line7\n"));
    }

    #[test]
    fn head_only_marker_at_end() {
        let input = "a\nb\nc\nd\ne\n".to_string();
        let result = bound_with_limits(input, TruncateMode::Head, 2, 1024);
        // Head-only: marker is the suffix after the kept prefix.
        assert!(result.truncated);
        assert!(result.content.starts_with("a\nb\n"));
        assert!(result.content.contains("[truncated"));
    }

    #[test]
    fn head_keeps_byte_prefix_of_overlong_single_line() {
        // Minified JSON-style single line bigger than the byte cap. The
        // previous implementation returned only the marker; now we keep a
        // byte-bounded prefix.
        let input = "x".repeat(2000);
        let result = bound_with_limits(input, TruncateMode::Head, 1000, 200);
        let marker_start = result.content.find("\n[truncated").expect("marker present");
        let body = &result.content[..marker_start];
        assert_eq!(body.len(), 200);
        assert!(body.chars().all(|c| c == 'x'));
        assert!(result.content.contains("1800 bytes"));
        assert!(result.truncated);
    }

    #[test]
    fn head_tail_keeps_byte_prefix_of_overlong_head_line() {
        // Long first line + a tail. Head budget keeps a prefix of the long
        // line; tail keeps the trailing lines untouched.
        let mut input = String::new();
        input.push_str(&"a".repeat(5000));
        input.push('\n');
        for i in 0..5 {
            input.push_str(&format!("tail{i}\n"));
        }
        let result = bound_with_limits(input, TruncateMode::HeadTail { head_lines: 2 }, 10, 1000);
        assert!(result.truncated);
        let marker_start = result.content.find("\n[truncated").expect("marker present");
        let head = &result.content[..marker_start];
        assert!(
            head.len() >= 100,
            "head was {} bytes: {:?}",
            head.len(),
            head
        );
        assert!(head.chars().all(|c| c == 'a'));
        // Tail still has the short trailing lines.
        let tail = &result.content[marker_start..];
        assert!(tail.contains("tail4\n"));
    }

    #[test]
    fn head_tail_keeps_byte_suffix_of_overlong_tail_line() {
        // Short head + a single huge final line that exceeds the tail
        // budget. Keep the trailing bytes (often where the error/result is).
        let mut input = String::new();
        for i in 0..3 {
            input.push_str(&format!("head{i}\n"));
        }
        input.push_str(&"z".repeat(3000));
        let result = bound_with_limits(input, TruncateMode::HeadTail { head_lines: 3 }, 10, 500);
        assert!(result.truncated);
        let marker_start = result.content.find("\n[truncated").expect("marker present");
        let head = &result.content[..marker_start];
        let tail = &result.content[marker_start..];
        assert!(head.contains("head0\n"));
        let z_count = tail.chars().filter(|c| *c == 'z').count();
        assert!(z_count > 50, "tail kept only {z_count} 'z' chars: {tail:?}");
    }

    #[test]
    fn byte_prefix_respects_utf8_boundaries() {
        // "é" is two bytes (0xC3 0xA9). Asking for a 1-byte prefix must
        // back up to a char boundary, not slice mid-codepoint.
        assert_eq!(byte_prefix("éé", 1), "");
        assert_eq!(byte_prefix("éé", 2), "é");
        assert_eq!(byte_prefix("éé", 3), "é");
        assert_eq!(byte_prefix("éé", 4), "éé");
    }

    #[test]
    fn byte_suffix_respects_utf8_boundaries() {
        assert_eq!(byte_suffix("éé", 1), "");
        assert_eq!(byte_suffix("éé", 2), "é");
        assert_eq!(byte_suffix("éé", 3), "é");
        assert_eq!(byte_suffix("éé", 4), "éé");
    }

    #[test]
    fn truncate_line_passes_through_under_budget() {
        let (text, was) = truncate_line("short line", 100);
        assert_eq!(text, "short line");
        assert!(!was);
    }

    #[test]
    fn truncate_line_clips_over_budget_with_suffix() {
        let input = "x".repeat(700);
        let (text, was) = truncate_line(&input, 500);
        assert!(was);
        let suffix = "... [truncated]";
        assert!(text.ends_with(suffix));
        let body = &text[..text.len() - suffix.len()];
        assert_eq!(body.len(), 500);
        assert!(body.chars().all(|c| c == 'x'));
    }

    #[test]
    fn truncate_line_respects_utf8_boundary() {
        // "é" is two bytes (0xC3 0xA9). Asking for a 1-byte budget on "ééé"
        // must back up to a char boundary so we never produce invalid UTF-8.
        let (text, was) = truncate_line("ééé", 1);
        assert!(was);
        let suffix = "... [truncated]";
        assert!(text.ends_with(suffix));
        let body = &text[..text.len() - suffix.len()];
        // body is either "" (backed off below 1 byte) — valid UTF-8 either way.
        assert!(body.is_empty() || body == "é");
    }

    #[test]
    fn head_tail_marker_between_segments() {
        let input = (0..10).map(|i| format!("{i}\n")).collect::<String>();
        let result = bound_with_limits(input, TruncateMode::HeadTail { head_lines: 2 }, 4, 10_000);
        assert!(result.truncated);
        let head_end = result.content.find("[truncated").expect("marker present");
        let head_part = &result.content[..head_end];
        let tail_part = &result.content[head_end..];
        // Head segment is the prefix; tail segment must come *after* the marker.
        assert!(head_part.contains("0\n"));
        assert!(head_part.contains("1\n"));
        assert!(tail_part.contains("9\n"));
        assert!(tail_part.contains("8\n"));
    }

    #[test]
    fn read_file_cap_is_stricter_than_generic() {
        // The whole point of the dedicated cap: stricter than the generic
        // tool-output cap so read_file can't dump 2000 lines of context.
        const _: () = assert!(READ_FILE_MAX_LINES < MAX_LINES);
        const _: () = assert!(READ_FILE_MAX_BYTES <= MAX_BYTES);
    }

    #[test]
    fn read_file_cap_truncates_long_files() {
        let input = (0..1000).map(|i| format!("line{i}\n")).collect::<String>();
        let result = bound_with_limits(
            input,
            TruncateMode::Head,
            READ_FILE_MAX_LINES,
            READ_FILE_MAX_BYTES,
        );
        assert!(result.truncated);
        assert!(result.content.contains("[truncated"));
        // Kept the first READ_FILE_MAX_LINES lines.
        assert!(result.content.contains("line0\n"));
        assert!(result.content.contains("line499\n"));
        assert!(!result.content.contains("line500\n"));
    }
}
