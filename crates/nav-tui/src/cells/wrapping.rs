use ratatui::text::Line;

/// Soft-wrap `text` to `width - 2` columns and prefix each line with a
/// two-space indent. A trailing newline is stripped so callers that
/// concatenate slices (e.g. stable + tail in a stream) don't see a phantom
/// blank line at the join.
///
/// Wrapping is word-aware: breaks prefer the last whitespace within the
/// available width, and the breaking whitespace run is consumed (so wrapped
/// continuations don't gain a stray leading space). A single word longer
/// than `body_width` falls back to a hard cut so we always make progress.
/// Counted in `char`s — wide / combining graphemes are out of scope for now.
pub(crate) fn render_body(text: &str, width: u16) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let body_width = width.saturating_sub(2) as usize;
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let mut out = Vec::new();
    for raw_line in trimmed.split('\n') {
        for chunk in wrap_line(raw_line, body_width) {
            out.push(body_line(chunk));
        }
    }
    out
}

fn body_line(text: &str) -> Line<'static> {
    Line::from(format!("  {text}"))
}

/// Count the wrapped lines `render_body` would produce. Stays in lockstep with
/// `render_body` by sharing [`wrap_line`]; the line-count is the dominant
/// signal for streaming-cell viewport sizing, so any drift here would let the
/// composer overlap painted history.
pub(crate) fn count_wrapped_body_lines(text: &str, width: u16) -> usize {
    if text.is_empty() {
        return 0;
    }
    let body_width = width.saturating_sub(2) as usize;
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let mut total = 0;
    for raw_line in trimmed.split('\n') {
        total += wrap_line(raw_line, body_width).len();
    }
    total
}

/// Split `raw_line` into wrapped chunks at `body_width` chars, preferring a
/// trailing whitespace break and consuming that whitespace run so it doesn't
/// reappear at the head of the next chunk. Returns one empty chunk for an
/// empty input so callers can preserve blank lines verbatim.
fn wrap_line(raw_line: &str, body_width: usize) -> Vec<&str> {
    if raw_line.is_empty() {
        return vec![""];
    }
    if body_width == 0 {
        return vec![raw_line];
    }

    let chars: Vec<(usize, char)> = raw_line.char_indices().collect();
    if chars.len() <= body_width {
        return vec![raw_line];
    }

    let mut chunks = Vec::new();
    let mut cursor = 0usize;

    while cursor < chars.len() {
        let remaining = chars.len() - cursor;
        if remaining <= body_width {
            chunks.push(&raw_line[chars[cursor].0..]);
            break;
        }

        // First char that would overflow. Scanning back from here (inclusive)
        // lets a whitespace sitting exactly at the column edge act as a clean
        // break, instead of being pushed onto the next line.
        let overflow_at = cursor + body_width;
        let break_at = (cursor + 1..=overflow_at)
            .rev()
            .find(|&i| chars[i].1.is_whitespace());

        match break_at {
            Some(ws_idx) => {
                chunks.push(&raw_line[chars[cursor].0..chars[ws_idx].0]);
                // Consume the run of whitespace so the wrapped continuation
                // starts on its first non-space character.
                let mut next = ws_idx;
                while next < chars.len() && chars[next].1.is_whitespace() {
                    next += 1;
                }
                cursor = next;
            }
            None => {
                // Single token longer than body_width — hard cut so we don't
                // loop forever. Continuation may still split mid-word.
                chunks.push(&raw_line[chars[cursor].0..chars[overflow_at].0]);
                cursor = overflow_at;
            }
        }
    }

    if chunks.is_empty() {
        // Defensive: only reachable if `raw_line` is all-whitespace longer
        // than `body_width` and the skip-loop ate every char. Emit one empty
        // chunk so the caller still gets a row.
        chunks.push("");
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(text: &str, width: u16) -> Vec<String> {
        render_body(text, width)
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn wraps_at_whitespace_not_mid_word() {
        // body_width = 20 - 2 = 18. The single hard cut at column 18 would
        // land inside "coding-agent" — we should break at the prior space.
        let out = rendered("nav is a Rust coding-agent harness built to last", 20);
        for line in &out {
            assert!(
                !line.contains("coding-age") || line.contains("coding-agent"),
                "wrapped line splits the word `coding-agent`: {line:?}"
            );
        }
    }

    #[test]
    fn consumes_breaking_whitespace() {
        let out = rendered("alpha bravo charlie delta", 14);
        // body_width = 12. No continuation should begin with a stray space.
        for line in out.iter().skip(1) {
            assert!(
                line.starts_with("  ") && !line.starts_with("   "),
                "continuation kept a leading space: {line:?}"
            );
        }
    }

    #[test]
    fn long_token_falls_back_to_hard_cut() {
        // body_width = 8. Single token of 20 chars must still wrap somehow.
        let out = rendered("aaaaaaaaaaaaaaaaaaaa", 10);
        assert!(out.len() >= 2, "expected hard-cut to split: {out:?}");
        for line in &out {
            // Each line is "  " indent + at most body_width chars of payload.
            assert!(line.len() <= 10, "line wider than width: {line:?}");
        }
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(rendered("", 40).is_empty());
        assert_eq!(count_wrapped_body_lines("", 40), 0);
    }

    #[test]
    fn blank_line_preserved_as_empty_row() {
        let out = rendered("alpha\n\nbravo", 40);
        assert_eq!(out, vec!["  alpha", "  ", "  bravo"]);
        assert_eq!(count_wrapped_body_lines("alpha\n\nbravo", 40), 3);
    }

    #[test]
    fn count_matches_render() {
        let cases = [
            "",
            "short",
            "alpha\nbravo\ncharlie",
            "nav is a Rust coding-agent harness built to last",
            "supercalifragilisticexpialidocious",
            "  leading indent that overflows the body width quite easily indeed",
            "trailing space  \nnext line",
        ];
        for case in cases {
            for width in [4u16, 10, 20, 40, 80] {
                assert_eq!(
                    render_body(case, width).len(),
                    count_wrapped_body_lines(case, width),
                    "count/render drift for case {case:?} at width {width}",
                );
            }
        }
    }

    #[test]
    fn body_width_zero_emits_one_line_per_segment() {
        // width <= 2 collapses body_width to 0; we just emit each raw line
        // verbatim under the two-space indent.
        let out = rendered("alpha\nbravo", 2);
        assert_eq!(out, vec!["  alpha", "  bravo"]);
    }
}
