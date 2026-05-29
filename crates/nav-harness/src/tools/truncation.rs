//! Shared output truncation utilities for filesystem and process tools.

pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const MAX_LINES: usize = 2000;
pub const TRUNCATED_MARKER: &str = "... [truncated]";

/// Per-tool char caps for model-visible output.
///
/// Counts Unicode scalar values (`.chars().count()`), not grapheme clusters.
pub const READ_MAX_CHARS: usize = 4000;
pub const BASH_MAX_CHARS: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationStrategy {
    Head,
    Tail,
    HeadTail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncationLimit {
    Bytes,
    Lines,
    Chars,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationOptions {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub max_chars: usize,
    pub strategy: TruncationStrategy,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_lines: MAX_LINES,
            max_chars: usize::MAX,
            strategy: TruncationStrategy::Head,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncatedOutput {
    segments: TruncatedSegments,
    truncated: bool,
    truncated_by: Option<TruncationLimit>,
    total_lines: usize,
    total_bytes: usize,
    output_lines: usize,
    output_bytes: usize,
    strategy: TruncationStrategy,
}

impl TruncatedOutput {
    pub fn content(&self) -> String {
        self.segments.content()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn truncated_by(&self) -> Option<TruncationLimit> {
        self.truncated_by
    }

    pub fn total_lines(&self) -> usize {
        self.total_lines
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn output_lines(&self) -> usize {
        self.output_lines
    }

    pub fn output_bytes(&self) -> usize {
        self.output_bytes
    }

    pub fn strategy(&self) -> TruncationStrategy {
        self.strategy
    }

    pub fn render(&self) -> String {
        if !self.truncated {
            return self.content();
        }

        match &self.segments {
            TruncatedSegments::Single(content) => match self.strategy {
                TruncationStrategy::Head => append_marker(content),
                TruncationStrategy::Tail => prepend_marker(content),
                TruncationStrategy::HeadTail => append_marker(content),
            },
            TruncatedSegments::HeadTail { head, tail } => render_head_tail(head, tail),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TruncatedSegments {
    Single(String),
    HeadTail { head: String, tail: String },
}

impl TruncatedSegments {
    fn content(&self) -> String {
        match self {
            Self::Single(content) => content.clone(),
            Self::HeadTail { head, tail } if head.is_empty() => tail.clone(),
            Self::HeadTail { head, tail } if tail.is_empty() => head.clone(),
            Self::HeadTail { head, tail } => format!("{head}\n{tail}"),
        }
    }
}

pub fn truncate_output(content: &str, options: TruncationOptions) -> TruncatedOutput {
    match options.strategy {
        TruncationStrategy::Head => truncate_head(content, options),
        TruncationStrategy::Tail => truncate_tail(content, options),
        TruncationStrategy::HeadTail => truncate_head_tail(content, options),
    }
}

pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncatedOutput {
    let total = ContentStats::new(content);
    if total.within(options) {
        return unchanged_output(content, total, options.strategy);
    }

    let mut output_lines = Vec::new();
    let mut output_bytes = 0;
    let mut output_chars = 0;
    let mut truncated_by = TruncationLimit::Lines;

    for (index, line) in content.split('\n').enumerate() {
        if output_lines.len() >= options.max_lines {
            break;
        }

        let line_bytes = line.len() + usize::from(index > 0);
        let line_chars = line.chars().count() + usize::from(index > 0);
        if output_bytes + line_bytes > options.max_bytes {
            truncated_by = TruncationLimit::Bytes;
            break;
        }
        if output_chars + line_chars > options.max_chars {
            truncated_by = TruncationLimit::Chars;
            if output_lines.is_empty() {
                output_lines.push(take_char_head(line, options.max_chars));
            }
            break;
        }

        output_lines.push(line);
        output_bytes += line_bytes;
        output_chars += line_chars;
    }

    let output = output_lines.join("\n");
    truncated_output(
        TruncatedSegments::Single(output),
        Some(truncated_by),
        total,
        options.strategy,
    )
}

pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncatedOutput {
    let total = ContentStats::new(content);
    if total.within(options) {
        return unchanged_output(content, total, options.strategy);
    }

    let mut lines = split_lines_for_tail(content);
    let mut output_lines = Vec::new();
    let mut output_bytes = 0;
    let mut output_chars = 0;
    let mut truncated_by = TruncationLimit::Lines;

    while let Some(line) = lines.pop() {
        if output_lines.len() >= options.max_lines {
            break;
        }

        let line_bytes = line.len() + usize::from(!output_lines.is_empty());
        let line_chars = line.chars().count() + usize::from(!output_lines.is_empty());
        if output_bytes + line_bytes > options.max_bytes {
            truncated_by = TruncationLimit::Bytes;
            if output_lines.is_empty() {
                output_lines.push(take_utf8_tail(line, options.max_bytes).to_string());
            }
            break;
        }
        if output_chars + line_chars > options.max_chars {
            truncated_by = TruncationLimit::Chars;
            if output_lines.is_empty() {
                output_lines.push(take_char_tail(line, options.max_chars).to_string());
            }
            break;
        }

        output_lines.push(line.to_string());
        output_bytes += line_bytes;
        output_chars += line_chars;
    }

    output_lines.reverse();
    let output = output_lines.join("\n");
    truncated_output(
        TruncatedSegments::Single(output),
        Some(truncated_by),
        total,
        options.strategy,
    )
}

pub fn truncate_head_tail(content: &str, options: TruncationOptions) -> TruncatedOutput {
    let total = ContentStats::new(content);
    if total.within(options) {
        return unchanged_output(content, total, options.strategy);
    }

    let head_lines = options.max_lines / 2;
    let tail_lines = options.max_lines.saturating_sub(head_lines);
    let head_bytes = options.max_bytes / 2;
    let tail_bytes = options.max_bytes.saturating_sub(head_bytes);
    let head = truncate_head(
        content,
        TruncationOptions {
            max_bytes: head_bytes,
            max_lines: head_lines,
            max_chars: options.max_chars / 2,
            strategy: TruncationStrategy::Head,
        },
    )
    .content();
    let tail = truncate_tail(
        content,
        TruncationOptions {
            max_bytes: tail_bytes,
            max_lines: tail_lines,
            max_chars: options.max_chars.saturating_sub(options.max_chars / 2),
            strategy: TruncationStrategy::Tail,
        },
    )
    .content();
    let truncated_by = if total.bytes > options.max_bytes {
        TruncationLimit::Bytes
    } else if total.chars > options.max_chars {
        TruncationLimit::Chars
    } else {
        TruncationLimit::Lines
    };

    truncated_output(
        TruncatedSegments::HeadTail { head, tail },
        Some(truncated_by),
        total,
        options.strategy,
    )
}

#[derive(Debug, Clone, Copy)]
struct ContentStats {
    lines: usize,
    bytes: usize,
    chars: usize,
}

impl ContentStats {
    fn new(content: &str) -> Self {
        Self {
            lines: count_lines(content),
            bytes: content.len(),
            chars: content.chars().count(),
        }
    }

    fn within(self, options: TruncationOptions) -> bool {
        self.lines <= options.max_lines
            && self.bytes <= options.max_bytes
            && self.chars <= options.max_chars
    }
}

fn unchanged_output(
    content: &str,
    total: ContentStats,
    strategy: TruncationStrategy,
) -> TruncatedOutput {
    TruncatedOutput {
        segments: TruncatedSegments::Single(content.to_string()),
        truncated: false,
        truncated_by: None,
        total_lines: total.lines,
        total_bytes: total.bytes,
        output_lines: total.lines,
        output_bytes: total.bytes,
        strategy,
    }
}

fn truncated_output(
    segments: TruncatedSegments,
    truncated_by: Option<TruncationLimit>,
    total: ContentStats,
    strategy: TruncationStrategy,
) -> TruncatedOutput {
    let content = segments.content();

    TruncatedOutput {
        segments,
        truncated: true,
        truncated_by,
        total_lines: total.lines,
        total_bytes: total.bytes,
        output_lines: count_lines(&content),
        output_bytes: content.len(),
        strategy,
    }
}

fn count_lines(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.split('\n').count()
    }
}

fn split_lines_for_tail(content: &str) -> Vec<&str> {
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if lines.len() > 1 && lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

fn take_utf8_tail(content: &str, max_bytes: usize) -> &str {
    if content.len() <= max_bytes {
        return content;
    }

    let mut start = content.len().saturating_sub(max_bytes);
    while start < content.len() && !content.is_char_boundary(start) {
        start += 1;
    }

    &content[start..]
}

fn take_char_head(content: &str, max_chars: usize) -> &str {
    if content.chars().count() <= max_chars {
        return content;
    }

    let end = content
        .char_indices()
        .nth(max_chars)
        .map_or(content.len(), |(i, _)| i);
    &content[..end]
}

fn take_char_tail(content: &str, max_chars: usize) -> &str {
    let total_chars = content.chars().count();
    if total_chars <= max_chars {
        return content;
    }

    let skip = total_chars - max_chars;
    let start = content
        .char_indices()
        .nth(skip)
        .map_or(content.len(), |(i, _)| i);
    &content[start..]
}

fn append_marker(content: &str) -> String {
    if content.is_empty() {
        TRUNCATED_MARKER.to_string()
    } else {
        format!("{content}\n{TRUNCATED_MARKER}")
    }
}

fn prepend_marker(content: &str) -> String {
    if content.is_empty() {
        TRUNCATED_MARKER.to_string()
    } else {
        format!("{TRUNCATED_MARKER}\n{content}")
    }
}

fn render_head_tail(head: &str, tail: &str) -> String {
    match (head.is_empty(), tail.is_empty()) {
        (true, true) => TRUNCATED_MARKER.to_string(),
        (true, false) => format!("{TRUNCATED_MARKER}\n{tail}"),
        (false, true) => format!("{head}\n{TRUNCATED_MARKER}"),
        (false, false) => format!("{head}\n{TRUNCATED_MARKER}\n{tail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_MAX_BYTES, MAX_LINES, TRUNCATED_MARKER, TruncationLimit, TruncationOptions,
        TruncationStrategy, truncate_output,
    };

    #[test]
    fn truncates_head_output_at_char_limit() {
        let content = "aaaa\nbbbb\ncccc\ndddd";
        let output = truncate_output(
            content,
            TruncationOptions {
                max_bytes: 1000,
                max_lines: 10,
                max_chars: 10,
                strategy: TruncationStrategy::Head,
            },
        );

        // "aaaa" (4) + "\n" (1) + "bbbb" (4) = 9 chars; next line would make 14
        assert_eq!(output.content(), "aaaa\nbbbb");
        assert!(output.truncated());
        assert_eq!(output.truncated_by(), Some(TruncationLimit::Chars));
    }

    #[test]
    fn char_limit_falls_back_to_partial_line_for_head() {
        let content = "abcdefghij\nsecond line";
        let output = truncate_output(
            content,
            TruncationOptions {
                max_bytes: 1000,
                max_lines: 10,
                max_chars: 5,
                strategy: TruncationStrategy::Head,
            },
        );

        // First line alone exceeds 5 chars — take char head of first line
        assert_eq!(output.content(), "abcde");
        assert!(output.truncated());
        assert_eq!(output.truncated_by(), Some(TruncationLimit::Chars));
    }

    #[test]
    fn char_limit_falls_back_to_partial_line_for_tail() {
        let content = "first line\nabcdefghij";
        let output = truncate_output(
            content,
            TruncationOptions {
                max_bytes: 1000,
                max_lines: 10,
                max_chars: 5,
                strategy: TruncationStrategy::Tail,
            },
        );

        // Last line alone exceeds 5 chars — take char tail of last line
        assert_eq!(output.content(), "fghij");
        assert!(output.truncated());
        assert_eq!(output.truncated_by(), Some(TruncationLimit::Chars));
    }

    #[test]
    fn max_chars_defaults_to_no_limit() {
        let options = TruncationOptions::default();
        assert_eq!(options.max_chars, usize::MAX);
    }

    #[test]
    fn exposes_pi_default_limits() {
        assert_eq!(DEFAULT_MAX_BYTES, 50 * 1024);
        assert_eq!(MAX_LINES, 2000);

        let options = TruncationOptions::default();
        assert_eq!(options.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(options.max_lines, MAX_LINES);
    }

    #[test]
    fn leaves_under_limit_output_unchanged() {
        let output = truncate_output(
            "short\noutput",
            TruncationOptions {
                max_bytes: 100,
                max_lines: 10,
                max_chars: usize::MAX,
                strategy: TruncationStrategy::Head,
            },
        );

        assert_eq!(output.content(), "short\noutput");
        assert!(!output.truncated());
        assert_eq!(output.render(), "short\noutput");
    }

    #[test]
    fn cuts_head_output_at_byte_limit_without_breaking_utf8() {
        let output = truncate_output(
            "éé\nabc",
            TruncationOptions {
                max_bytes: 4,
                max_lines: 10,
                max_chars: usize::MAX,
                strategy: TruncationStrategy::Head,
            },
        );

        assert_eq!(output.content(), "éé");
        assert!(output.truncated());
        assert_eq!(output.truncated_by(), Some(TruncationLimit::Bytes));
        assert_eq!(output.output_bytes(), 4);
    }

    #[test]
    fn cuts_head_output_at_line_limit() {
        let output = truncate_output(
            "one\ntwo\nthree",
            TruncationOptions {
                max_bytes: 100,
                max_lines: 2,
                max_chars: usize::MAX,
                strategy: TruncationStrategy::Head,
            },
        );

        assert_eq!(output.content(), "one\ntwo");
        assert!(output.truncated());
        assert_eq!(output.truncated_by(), Some(TruncationLimit::Lines));
        assert_eq!(output.output_lines(), 2);
    }

    #[test]
    fn keeps_tail_output_for_tail_strategy() {
        let output = truncate_output(
            "one\ntwo\nthree",
            TruncationOptions {
                max_bytes: 100,
                max_lines: 1,
                max_chars: usize::MAX,
                strategy: TruncationStrategy::Tail,
            },
        );

        assert_eq!(output.content(), "three");
        assert_eq!(output.render(), format!("{TRUNCATED_MARKER}\nthree"));
    }

    #[test]
    fn keeps_head_and_tail_for_split_strategy() {
        let output = truncate_output(
            "one\ntwo\nthree\nfour\nfive\nsix",
            TruncationOptions {
                max_bytes: 100,
                max_lines: 4,
                max_chars: usize::MAX,
                strategy: TruncationStrategy::HeadTail,
            },
        );

        assert_eq!(output.content(), "one\ntwo\nfive\nsix");
        assert_eq!(
            output.render(),
            format!("one\ntwo\n{TRUNCATED_MARKER}\nfive\nsix")
        );
        assert_eq!(output.strategy(), TruncationStrategy::HeadTail);
        assert_eq!(output.truncated_by(), Some(TruncationLimit::Lines));
    }

    #[test]
    fn marker_rendering_makes_head_truncation_visible() {
        let output = truncate_output(
            "one\ntwo",
            TruncationOptions {
                max_bytes: 3,
                max_lines: 10,
                max_chars: usize::MAX,
                strategy: TruncationStrategy::Head,
            },
        );

        assert_eq!(output.content(), "one");
        assert_eq!(output.render(), format!("one\n{TRUNCATED_MARKER}"));
        assert_eq!(output.total_lines(), 2);
        assert_eq!(output.total_bytes(), 7);
    }
}
