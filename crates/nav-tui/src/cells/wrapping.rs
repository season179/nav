use ratatui::text::Line;

/// Soft-wrap `text` to `width - 2` columns and prefix each line with a
/// two-space indent. A trailing newline is stripped so callers that
/// concatenate slices (e.g. stable + tail in a stream) don't see a phantom
/// blank line at the join. ASCII-only — grapheme-aware wrapping can replace
/// this one helper.
pub(crate) fn render_body(text: &str, width: u16) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let body_width = width.saturating_sub(2) as usize;
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let mut out = Vec::new();
    for raw_line in trimmed.split('\n') {
        append_wrapped_line(raw_line, body_width, &mut out);
    }
    out
}

fn append_wrapped_line(raw_line: &str, body_width: usize, out: &mut Vec<Line<'static>>) {
    if body_width == 0 {
        out.push(body_line(raw_line));
        return;
    }
    let mut chunk_start = 0;
    let mut count = 0;
    let mut produced = false;
    for (idx, _) in raw_line.char_indices() {
        if count == body_width {
            out.push(body_line(&raw_line[chunk_start..idx]));
            chunk_start = idx;
            count = 0;
            produced = true;
        }
        count += 1;
    }
    if !produced || chunk_start < raw_line.len() {
        out.push(body_line(&raw_line[chunk_start..]));
    }
}

fn body_line(text: &str) -> Line<'static> {
    Line::from(format!("  {text}"))
}

/// Count the wrapped lines `render_body` would produce, without allocating.
/// Mirrors the wrapping rules in [`render_body`] / [`append_wrapped_line`]:
/// empty input → 0; otherwise each `\n`-delimited segment contributes
/// `ceil(chars / body_width)` rows (minimum 1, even for empty segments and for
/// `body_width == 0`).
pub(crate) fn count_wrapped_body_lines(text: &str, width: u16) -> usize {
    if text.is_empty() {
        return 0;
    }
    let body_width = width.saturating_sub(2) as usize;
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let mut total = 0;
    for raw_line in trimmed.split('\n') {
        total += count_wrapped_segment(raw_line, body_width);
    }
    total
}

fn count_wrapped_segment(raw_line: &str, body_width: usize) -> usize {
    if body_width == 0 {
        return 1;
    }
    let chars = raw_line.chars().count();
    if chars == 0 {
        1
    } else {
        chars.div_ceil(body_width)
    }
}
