//! Shared output truncation utilities for filesystem and process tools.

pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const MAX_LINES: usize = 2000;
pub const TRUNCATED_MARKER: &str = "... [truncated]";

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationOptions {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub strategy: TruncationStrategy,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_lines: MAX_LINES,
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
    let mut truncated_by = TruncationLimit::Lines;

    for (index, line) in content.split('\n').enumerate() {
        if output_lines.len() >= options.max_lines {
            break;
        }

        let line_bytes = line.len() + usize::from(index > 0);
        if output_bytes + line_bytes > options.max_bytes {
            truncated_by = TruncationLimit::Bytes;
            break;
        }

        output_lines.push(line);
        output_bytes += line_bytes;
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
    let mut truncated_by = TruncationLimit::Lines;

    while let Some(line) = lines.pop() {
        if output_lines.len() >= options.max_lines {
            break;
        }

        let line_bytes = line.len() + usize::from(!output_lines.is_empty());
        if output_bytes + line_bytes > options.max_bytes {
            truncated_by = TruncationLimit::Bytes;
            if output_lines.is_empty() {
                output_lines.push(take_utf8_tail(line, options.max_bytes).to_string());
            }
            break;
        }

        output_lines.push(line.to_string());
        output_bytes += line_bytes;
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
            strategy: TruncationStrategy::Head,
        },
    )
    .content();
    let tail = truncate_tail(
        content,
        TruncationOptions {
            max_bytes: tail_bytes,
            max_lines: tail_lines,
            strategy: TruncationStrategy::Tail,
        },
    )
    .content();
    let truncated_by = if total.bytes > options.max_bytes {
        TruncationLimit::Bytes
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
}

impl ContentStats {
    fn new(content: &str) -> Self {
        Self {
            lines: count_lines(content),
            bytes: content.len(),
        }
    }

    fn within(self, options: TruncationOptions) -> bool {
        self.lines <= options.max_lines && self.bytes <= options.max_bytes
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
        truncate_output, TruncationLimit, TruncationOptions, TruncationStrategy, DEFAULT_MAX_BYTES,
        MAX_LINES, TRUNCATED_MARKER,
    };

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
                strategy: TruncationStrategy::Head,
            },
        );

        assert_eq!(output.content(), "one");
        assert_eq!(output.render(), format!("one\n{TRUNCATED_MARKER}"));
        assert_eq!(output.total_lines(), 2);
        assert_eq!(output.total_bytes(), 7);
    }
}
