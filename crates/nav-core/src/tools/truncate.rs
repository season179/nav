//! Bounds tool output before it reaches the model prompt or session log.
//!
//! Without a cap, a single `bash` or `read_file` can blow up the context
//! window and inflate every subsequent turn (including the persisted SQLite
//! event log). This module applies a dual cap (`MAX_LINES` *or* `MAX_BYTES`,
//! whichever is hit first) and inserts a single human-readable marker noting
//! what was dropped. Bounding happens once at the tool boundary so the same
//! truncated `String` flows into both the prompt and the session log.

pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;

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
pub fn bound(output: String, mode: TruncateMode) -> String {
    bound_with_limits(output, mode, MAX_LINES, MAX_BYTES)
}

fn bound_with_limits(
    output: String,
    mode: TruncateMode,
    max_lines: usize,
    max_bytes: usize,
) -> String {
    let total_bytes = output.len();
    let lines: Vec<&str> = output.split_inclusive('\n').collect();
    let total_lines = lines.len();
    if total_bytes <= max_bytes && total_lines <= max_lines {
        return output;
    }

    match mode {
        TruncateMode::Head => render_head(&lines, max_lines, max_bytes, total_lines, total_bytes),
        TruncateMode::HeadTail { head_lines } => render_head_tail(
            &lines,
            head_lines,
            max_lines,
            max_bytes,
            total_lines,
            total_bytes,
        ),
    }
}

fn marker(dropped_bytes: usize, dropped_lines: usize) -> String {
    format!("\n[truncated {dropped_bytes} bytes / {dropped_lines} lines]\n")
}

fn render_head(
    lines: &[&str],
    max_lines: usize,
    max_bytes: usize,
    total_lines: usize,
    total_bytes: usize,
) -> String {
    let mut kept_text = String::new();
    let mut kept_lines = 0usize;
    let mut kept_bytes = 0usize;
    for line in lines {
        if kept_lines >= max_lines || kept_bytes + line.len() > max_bytes {
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
    result
}

fn render_head_tail(
    lines: &[&str],
    head_lines_budget: usize,
    max_lines: usize,
    max_bytes: usize,
    total_lines: usize,
    total_bytes: usize,
) -> String {
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
        if head_lines_kept >= head_lines_budget
            || head_bytes_kept + line.len() > head_byte_budget
        {
            break;
        }
        head_text.push_str(line);
        head_bytes_kept += line.len();
        head_lines_kept += 1;
    }

    // The tail starts after the lines the head already consumed, so a
    // small input never gets the same line in both segments.
    let remaining = &lines[head_lines_kept..];
    let mut tail_chunks: Vec<&str> = Vec::new();
    let mut tail_lines_kept = 0usize;
    let mut tail_bytes_kept = 0usize;
    for line in remaining.iter().rev() {
        if tail_lines_kept >= tail_lines_budget
            || tail_bytes_kept + line.len() > tail_byte_budget
        {
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
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_passes_through_unchanged() {
        let input = "hello\nworld\n".to_string();
        let result = bound(input.clone(), TruncateMode::Head);
        assert_eq!(result, input);
    }

    #[test]
    fn head_caps_at_max_lines() {
        let mut input = String::new();
        for i in 0..50 {
            input.push_str(&format!("line {i}\n"));
        }
        let result = bound_with_limits(input.clone(), TruncateMode::Head, 10, 1024);
        assert!(result.starts_with("line 0\n"));
        assert!(result.contains("[truncated"));
        assert!(result.contains("40 lines"));
        // None of the dropped lines should appear.
        assert!(!result.contains("line 10\n"));
    }

    #[test]
    fn head_caps_at_max_bytes() {
        let line = "x".repeat(100) + "\n";
        let input = line.repeat(50); // 5050 bytes
        let result = bound_with_limits(input, TruncateMode::Head, 1000, 500);
        assert!(result.len() < 1024); // 500 cap + marker
        assert!(result.contains("[truncated"));
        assert!(result.contains("bytes"));
    }

    #[test]
    fn head_marker_shows_dropped_counts() {
        let mut input = String::new();
        for _ in 0..30 {
            input.push_str("abc\n"); // 4 bytes each = 120 total
        }
        let result = bound_with_limits(input, TruncateMode::Head, 5, 1024);
        // 5 kept, 25 dropped; 5*4=20 kept, 100 dropped.
        assert!(result.contains("[truncated 100 bytes / 25 lines]"));
    }

    #[test]
    fn head_tail_keeps_first_and_last_lines() {
        let mut input = String::new();
        for i in 0..100 {
            input.push_str(&format!("{i}\n"));
        }
        let result = bound_with_limits(
            input,
            TruncateMode::HeadTail { head_lines: 3 },
            10,
            10_000,
        );
        // Head: 0,1,2
        assert!(result.starts_with("0\n1\n2\n"));
        // Tail: 93..99 (10 - 3 = 7 tail lines).
        assert!(result.contains("\n99\n"));
        assert!(result.contains("\n93\n"));
        // Middle should be gone.
        assert!(!result.contains("\n50\n"));
        assert!(result.contains("[truncated"));
    }

    #[test]
    fn head_tail_handles_byte_cap_before_line_cap() {
        // Build wide lines so byte budget hits before line budget.
        let line = "y".repeat(80) + "\n"; // 81 bytes
        let input = line.repeat(100); // 8100 bytes
        let result = bound_with_limits(
            input,
            TruncateMode::HeadTail { head_lines: 2 },
            50,
            500,
        );
        assert!(result.len() <= 600); // cap + marker headroom
        assert!(result.contains("[truncated"));
    }

    #[test]
    fn head_tail_does_not_overlap_head_and_tail_lines() {
        // 8 lines, asking for head=3 + tail=4 (max_lines=7). Line 3 must not
        // appear twice and line 7 must be present.
        let input = (0..8)
            .map(|i| format!("line{i}\n"))
            .collect::<String>();
        let result = bound_with_limits(
            input,
            TruncateMode::HeadTail { head_lines: 3 },
            7,
            10_000,
        );
        let occurrences = result.matches("line3\n").count();
        assert!(occurrences <= 1, "line3 appeared {occurrences} times: {result}");
        assert!(result.contains("line0\n"));
        assert!(result.contains("line7\n"));
    }

    #[test]
    fn head_only_marker_at_end() {
        let input = "a\nb\nc\nd\ne\n".to_string();
        let result = bound_with_limits(input, TruncateMode::Head, 2, 1024);
        // Head-only: marker is the suffix after the kept prefix.
        assert!(result.starts_with("a\nb\n"));
        assert!(result.contains("[truncated"));
    }

    #[test]
    fn head_tail_marker_between_segments() {
        let input = (0..10).map(|i| format!("{i}\n")).collect::<String>();
        let result = bound_with_limits(
            input,
            TruncateMode::HeadTail { head_lines: 2 },
            4,
            10_000,
        );
        let head_end = result.find("[truncated").expect("marker present");
        let head_part = &result[..head_end];
        let tail_part = &result[head_end..];
        // Head segment is the prefix; tail segment must come *after* the marker.
        assert!(head_part.contains("0\n"));
        assert!(head_part.contains("1\n"));
        assert!(tail_part.contains("9\n"));
        assert!(tail_part.contains("8\n"));
    }
}
