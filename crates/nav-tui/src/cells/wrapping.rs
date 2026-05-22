use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

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
    let spans = parse_inline_spans(text);
    let mut line_spans = vec![Span::raw("  ")];
    line_spans.extend(spans);
    Line::from(line_spans)
}

/// Parse inline markdown markers in a single wrapped chunk:
/// - `code` → colored yellow
/// - *bold* → bold
/// - _italic_ → italic
///
/// Unmatched opening markers are emitted as plain text.
/// The markers themselves are consumed (not rendered).
fn parse_inline_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&ch| ch == '`') {
                let close_abs = i + 1 + end;
                flush_plain(&mut spans, &mut plain);
                let code_text: String = chars[i + 1..close_abs].iter().collect();
                spans.push(Span::styled(
                    code_text,
                    Style::default().fg(Color::Yellow),
                ));
                i = close_abs + 1;
                continue;
            }
        } else if let Some(modifier) = match c {
            '*' => Some(Modifier::BOLD),
            '_' if is_flanking_open(&chars, i) => Some(Modifier::ITALIC),
            _ => None,
        } {
            if let Some(end) = find_closing(&chars, i, c)
                && (c != '_' || is_flanking_close(&chars, end))
            {
                flush_plain(&mut spans, &mut plain);
                let styled_text: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(
                    styled_text,
                    Style::default().add_modifier(modifier),
                ));
                i = end + 1;
                continue;
            }
        }
        plain.push(c);
        i += 1;
    }
    flush_plain(&mut spans, &mut plain);
    spans
}

/// Find the closing delimiter for an inline marker, ensuring it's not
/// immediately adjacent to the opener (empty pairs like `**` or `__`
/// are skipped) and not part of a doubled run like `**bold**`.
/// Returns the char index of the closing delimiter.
fn find_closing(chars: &[char], open: usize, delim: char) -> Option<usize> {
    // Empty pair check: opener at `open`, if next char is same delim, skip.
    if open + 1 < chars.len() && chars[open + 1] == delim {
        return None;
    }
    for j in (open + 1)..chars.len() {
        if chars[j] == delim && !is_doubled(chars, j, delim) {
            return Some(j);
        }
    }
    None
}

/// True when `chars[j]` is part of a doubled delimiter run (e.g. `**`).
fn is_doubled(chars: &[char], j: usize, delim: char) -> bool {
    (j + 1 < chars.len() && chars[j + 1] == delim)
        || (j > 0 && chars[j - 1] == delim)
}

/// True when `chars[i]` is `_` at a word boundary suitable for opening italic.
/// Following CommonMark §6.2: `_` opens emphasis only when preceded by a
/// non-word character (or start of string).
fn is_flanking_open(chars: &[char], i: usize) -> bool {
    i == 0 || !is_word_char(chars[i - 1])
}

/// True when `chars[j]` is `_` at a word boundary suitable for closing italic.
/// Following CommonMark §6.2: `_` closes emphasis only when followed by a
/// non-word character (or end of string).
fn is_flanking_close(chars: &[char], j: usize) -> bool {
    j + 1 >= chars.len() || !is_word_char(chars[j + 1])
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn flush_plain(spans: &mut Vec<Span<'static>>, plain: &mut String) {
    if !plain.is_empty() {
        spans.push(Span::raw(std::mem::take(plain)));
    }
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

    // --- Inline markdown formatting tests ---

    fn span_contents(text: &str) -> Vec<String> {
        let lines = render_body(text, 80);
        lines
            .into_iter()
            .flat_map(|line| {
                line.spans
                    .into_iter()
                    .map(|s| s.content.to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn backtick_code_is_colored_yellow() {
        use ratatui::style::Color;
        let lines = render_body("use `cargo test` to run", 80);
        // Find a yellow span
        let code_span = lines[0]
            .spans
            .iter()
            .find(|s| s.style.fg == Some(Color::Yellow));
        assert!(code_span.is_some(), "no yellow span found");
        assert_eq!(code_span.unwrap().content, "cargo test");
        // The backticks themselves are gone
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "  use cargo test to run");
    }

    #[test]
    fn asterisk_bold() {
        use ratatui::style::Modifier;
        let lines = render_body("this is *important* ok", 80);
        let bold_span = lines[0]
            .spans
            .iter()
            .find(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(bold_span.is_some(), "no bold span found");
        assert_eq!(bold_span.unwrap().content, "important");
    }

    #[test]
    fn underscore_italic() {
        use ratatui::style::Modifier;
        let lines = render_body("this is _emphasized_ ok", 80);
        let italic_span = lines[0]
            .spans
            .iter()
            .find(|s| s.style.add_modifier.contains(Modifier::ITALIC));
        assert!(italic_span.is_some(), "no italic span found");
        assert_eq!(italic_span.unwrap().content, "emphasized");
    }

    #[test]
    fn unmatched_delimiter_is_plain_text() {
        let lines = render_body("price is $5 *each", 80);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "  price is $5 *each");
    }

    #[test]
    fn empty_code_span_produces_nothing() {
        let lines = render_body("before `` after", 80);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "  before  after");
    }

    #[test]
    fn adjacent_formats_dont_merge() {
        use ratatui::style::{Color, Modifier};
        let lines = render_body("`code` and *bold*", 80);
        let spans = &lines[0].spans;
        assert!(spans.iter().any(|s| s.style.fg == Some(Color::Yellow)));
        assert!(spans.iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn underscore_mid_word_is_not_italic() {
        // CommonMark §6.2: _ only triggers at word boundaries
        let lines = render_body("use a_b_c here", 80);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "  use a_b_c here");
    }

    #[test]
    fn underscore_at_word_boundary_is_italic() {
        use ratatui::style::Modifier;
        let lines = render_body("it is _really_ fine", 80);
        let italic_span = lines[0]
            .spans
            .iter()
            .find(|s| s.style.add_modifier.contains(Modifier::ITALIC));
        assert!(italic_span.is_some(), "no italic span found");
        assert_eq!(italic_span.unwrap().content, "really");
    }
}
