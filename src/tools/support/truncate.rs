//! Output caps shared by every tool. One tool call must never exceed these, or
//! it could blow the model's context window and break every later turn in the
//! run (and bloat the SQLite row).

/// Maximum lines kept from a single tool's output.
pub const MAX_LINES: usize = 2000;
/// Maximum bytes kept from a single tool's output.
pub const MAX_BYTES: usize = 50 * 1024;

/// Marker appended when output was clipped.
pub const TRUNCATION_MARKER: &str = "\n… [output truncated]";

/// Cap output to the head (first lines/bytes) — for file reads and listings.
pub fn cap_head(text: &str) -> String {
    cap(text, false)
}

/// Cap output to the tail (last lines/bytes) — for command output where the
/// end is usually the interesting part.
pub fn cap_tail(text: &str) -> String {
    cap(text, true)
}

fn cap(text: &str, keep_tail: bool) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut truncated = false;

    let kept: Vec<&str> = if lines.len() > MAX_LINES {
        truncated = true;
        if keep_tail {
            lines[lines.len() - MAX_LINES..].to_vec()
        } else {
            lines[..MAX_LINES].to_vec()
        }
    } else {
        lines
    };

    let mut out = kept.join("\n");

    if out.len() > MAX_BYTES {
        truncated = true;
    }

    // When a marker will be appended, clip to leave room for it so the final
    // string (content + marker) never exceeds MAX_BYTES.
    let budget = if truncated {
        MAX_BYTES.saturating_sub(TRUNCATION_MARKER.len())
    } else {
        MAX_BYTES
    };
    if out.len() > budget {
        if keep_tail {
            let start = floor_char_boundary(&out, out.len() - budget);
            out = out[start..].to_owned();
        } else {
            let end = floor_char_boundary(&out, budget);
            out = out[..end].to_owned();
        }
    }

    if truncated {
        out.push_str(TRUNCATION_MARKER);
    }
    out
}

/// Largest char boundary `<= index` (std's `floor_char_boundary` is unstable).
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut boundary = index;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_is_unchanged() {
        assert_eq!(cap_head("a\nb\nc"), "a\nb\nc");
    }

    #[test]
    fn head_cap_keeps_the_first_lines_and_marks_truncation() {
        let text = (0..MAX_LINES + 50)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let capped = cap_head(&text);
        assert!(capped.starts_with("0\n1\n2"));
        assert!(capped.ends_with(TRUNCATION_MARKER));
        // The kept lines are the first MAX_LINES, not the last.
        assert!(capped.contains(&format!("\n{}\n", MAX_LINES - 1)));
        assert!(!capped.contains(&format!("\n{}\n", MAX_LINES + 10)));
    }

    #[test]
    fn tail_cap_keeps_the_last_lines() {
        let text = (0..MAX_LINES + 50)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let capped = cap_tail(&text);
        assert!(capped.contains(&(MAX_LINES + 49).to_string()));
        assert!(capped.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn byte_cap_respects_char_boundaries() {
        let text = "✓".repeat(MAX_BYTES); // 3 bytes each, well over the cap
        let capped = cap_head(&text);
        // Must still be valid UTF-8 (no panic on slicing) and marked truncated.
        assert!(capped.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn final_output_never_exceeds_the_byte_cap() {
        // The marker must fit within MAX_BYTES, not push the result past it.
        let head = cap_head(&"x".repeat(MAX_BYTES * 2));
        assert!(head.len() <= MAX_BYTES, "head len {}", head.len());
        assert!(head.ends_with(TRUNCATION_MARKER));

        let tail = cap_tail(&"y".repeat(MAX_BYTES * 2));
        assert!(tail.len() <= MAX_BYTES, "tail len {}", tail.len());
        assert!(tail.ends_with(TRUNCATION_MARKER));
    }
}
