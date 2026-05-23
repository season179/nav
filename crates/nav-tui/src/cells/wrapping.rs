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
    let mut in_fence = false;
    for raw_line in trimmed.split('\n') {
        let fence = is_fence_line(raw_line);
        if fence {
            in_fence = !in_fence;
        }
        for chunk in wrap_line(raw_line, body_width) {
            if in_fence || fence {
                out.push(code_block_line(chunk));
            } else {
                out.push(body_line(chunk));
            }
        }
    }
    out
}

/// A line that opens or closes a triple-backtick fenced code block.
fn is_fence_line(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn body_line(text: &str) -> Line<'static> {
    let spans = parse_inline_spans(text);
    let mut line_spans = vec![Span::raw("  ")];
    line_spans.extend(spans);
    Line::from(line_spans)
}

/// Render a line inside a fenced code block — entire content is cyan and
/// inline markers are not interpreted.
fn code_block_line(text: &str) -> Line<'static> {
    let mut line_spans = vec![Span::raw("  ")];
    if !text.is_empty() {
        line_spans.push(Span::styled(
            text.to_string(),
            Style::default().fg(Color::Cyan),
        ));
    }
    Line::from(line_spans)
}

/// Parse inline markdown markers in a single wrapped chunk:
/// - `code` and ```code``` (any backtick run) → colored cyan
/// - *bold* and **bold** → bold
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
            let run = delim_run(&chars, i, '`');
            if let Some(close) = find_run_close(&chars, i + run, run, '`') {
                push_styled_span(
                    &mut spans,
                    &mut plain,
                    &chars[i + run..close],
                    Style::default().fg(Color::Cyan),
                );
                i = close + run;
                continue;
            }
        } else if c == '*' {
            let run = if i + 1 < chars.len() && chars[i + 1] == '*' {
                2
            } else {
                1
            };
            if let Some(close) = find_run_close(&chars, i + run, run, '*') {
                push_styled_span(
                    &mut spans,
                    &mut plain,
                    &chars[i + run..close],
                    Style::default().add_modifier(Modifier::BOLD),
                );
                i = close + run;
                continue;
            }
        } else if c == '_'
            && is_flanking_open(&chars, i)
            && let Some(end) = find_underscore_close(&chars, i)
            && is_flanking_close(&chars, end)
        {
            push_styled_span(
                &mut spans,
                &mut plain,
                &chars[i + 1..end],
                Style::default().add_modifier(Modifier::ITALIC),
            );
            i = end + 1;
            continue;
        }
        plain.push(c);
        i += 1;
    }
    flush_plain(&mut spans, &mut plain);
    spans
}

fn push_styled_span(
    spans: &mut Vec<Span<'static>>,
    plain: &mut String,
    chars: &[char],
    style: Style,
) {
    flush_plain(spans, plain);
    spans.push(Span::styled(chars.iter().collect::<String>(), style));
}

/// Length of the run of `delim` characters starting at `start`.
fn delim_run(chars: &[char], start: usize, delim: char) -> usize {
    chars[start..].iter().take_while(|&&c| c == delim).count()
}

/// Find the next run of exactly `len` `delim` chars starting at or after
/// `from`. A run is "exact" when it isn't extended by another `delim`
/// immediately to its right — so a length-2 search won't bind to the
/// leading `**` inside a `***` run.
fn find_run_close(chars: &[char], from: usize, len: usize, delim: char) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let mut j = from;
    while j + len <= chars.len() {
        if chars[j] == delim
            && (0..len).all(|k| chars[j + k] == delim)
            && !(j + len < chars.len() && chars[j + len] == delim)
        {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Find the closing `_` for an italic opener at `open`. Empty pairs and
/// `__` doubled runs are skipped — bold/strong via `__` is not supported.
fn find_underscore_close(chars: &[char], open: usize) -> Option<usize> {
    if open + 1 < chars.len() && chars[open + 1] == '_' {
        return None;
    }
    ((open + 1)..chars.len()).find(|&j| chars[j] == '_' && !is_doubled(chars, j, '_'))
}

/// True when `chars[j]` is part of a doubled delimiter run (e.g. `__`).
fn is_doubled(chars: &[char], j: usize, delim: char) -> bool {
    (j + 1 < chars.len() && chars[j + 1] == delim) || (j > 0 && chars[j - 1] == delim)
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
    use ratatui::style::{Color, Modifier};

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

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn cyan_span<'a, 'line>(line: &'a Line<'line>) -> Option<&'a Span<'line>> {
        line.spans.iter().find(|s| s.style.fg == Some(Color::Cyan))
    }

    fn modifier_span<'a, 'line>(
        line: &'a Line<'line>,
        modifier: Modifier,
    ) -> Option<&'a Span<'line>> {
        line.spans
            .iter()
            .find(|s| s.style.add_modifier.contains(modifier))
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

    #[test]
    fn backtick_code_is_colored_cyan() {
        let lines = render_body("use `cargo test` to run", 80);
        let code_span = cyan_span(&lines[0]).expect("no cyan span found");
        assert_eq!(code_span.content, "cargo test");
        // The backticks themselves are gone
        assert_eq!(line_text(&lines[0]), "  use cargo test to run");
    }

    #[test]
    fn inline_triple_backtick_is_colored_cyan() {
        let lines = render_body("see ```cargo run``` here", 80);
        let code_span = cyan_span(&lines[0]).expect("no cyan span found");
        assert_eq!(code_span.content, "cargo run");
        assert_eq!(line_text(&lines[0]), "  see cargo run here");
    }

    #[test]
    fn fenced_code_block_is_colored_cyan() {
        let input = "before\n```rust\nfn foo() {}\nlet x = 1;\n```\nafter";
        let lines = render_body(input, 80);
        // Lines: "before", "```rust", "fn foo() {}", "let x = 1;", "```", "after".
        let fenced_indices = [1usize, 2, 3, 4];
        for &idx in &fenced_indices {
            let has_cyan = cyan_span(&lines[idx]).is_some();
            assert!(
                has_cyan,
                "expected cyan span on line {idx}: {:?}",
                lines[idx]
            );
        }
        // Surrounding prose lines stay un-cyan.
        for &idx in &[0usize, 5] {
            let any_cyan = cyan_span(&lines[idx]).is_some();
            assert!(
                !any_cyan,
                "unexpected cyan on prose line {idx}: {:?}",
                lines[idx]
            );
        }
    }

    #[test]
    fn double_asterisk_bold() {
        let lines = render_body("this is **important** ok", 80);
        let bold_span = modifier_span(&lines[0], Modifier::BOLD).expect("no bold span found");
        assert_eq!(bold_span.content, "important");
        assert_eq!(line_text(&lines[0]), "  this is important ok");
    }

    #[test]
    fn asterisk_bold() {
        let lines = render_body("this is *important* ok", 80);
        let bold_span = modifier_span(&lines[0], Modifier::BOLD).expect("no bold span found");
        assert_eq!(bold_span.content, "important");
    }

    #[test]
    fn underscore_italic() {
        let lines = render_body("this is _emphasized_ ok", 80);
        let italic_span = modifier_span(&lines[0], Modifier::ITALIC).expect("no italic span found");
        assert_eq!(italic_span.content, "emphasized");
    }

    #[test]
    fn unmatched_delimiter_is_plain_text() {
        let lines = render_body("price is $5 *each", 80);
        assert_eq!(line_text(&lines[0]), "  price is $5 *each");
    }

    #[test]
    fn unmatched_double_backticks_render_literally() {
        // `` is a length-2 opener with no length-2 closer in the line, so
        // the backticks fall through as plain text rather than producing
        // an empty styled span.
        let lines = render_body("before `` after", 80);
        assert_eq!(line_text(&lines[0]), "  before `` after");
    }

    #[test]
    fn adjacent_formats_dont_merge() {
        let lines = render_body("`code` and *bold*", 80);
        assert!(cyan_span(&lines[0]).is_some());
        assert!(modifier_span(&lines[0], Modifier::BOLD).is_some());
    }

    #[test]
    fn underscore_mid_word_is_not_italic() {
        // CommonMark §6.2: _ only triggers at word boundaries
        let lines = render_body("use a_b_c here", 80);
        assert_eq!(line_text(&lines[0]), "  use a_b_c here");
    }

    #[test]
    fn underscore_at_word_boundary_is_italic() {
        let lines = render_body("it is _really_ fine", 80);
        let italic_span = modifier_span(&lines[0], Modifier::ITALIC).expect("no italic span found");
        assert_eq!(italic_span.content, "really");
    }
}
