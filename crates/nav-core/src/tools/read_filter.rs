//! RTK-style source filtering for rewritten `cat`/`head`/`tail` calls.
//!
//! This mirrors the behavior of RTK's `read` command: detect a file's language,
//! strip comments/blank noise with the minimal filter, then apply the same
//! smart line window used by `rtk read --max-lines`.

use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    C,
    Cpp,
    Java,
    Ruby,
    Shell,
    Data,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ReadOptions {
    pub(super) max_lines: Option<usize>,
    pub(super) tail_lines: Option<usize>,
    pub(super) line_numbers: bool,
}

impl ReadOptions {
    pub(super) fn minimal() -> Self {
        Self {
            max_lines: None,
            tail_lines: None,
            line_numbers: false,
        }
    }
}

#[derive(Debug, Clone)]
struct CommentPatterns {
    line: Option<&'static str>,
    block_start: Option<&'static str>,
    block_end: Option<&'static str>,
    doc_line: Option<&'static str>,
    doc_block_start: Option<&'static str>,
}

static MULTIPLE_BLANK_LINES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").expect("valid blank-line regex"));
static IMPORT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(use |import |from |require\(|#include)").expect("valid import regex")
});
static FUNC_SIGNATURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(pub\s+)?(async\s+)?(fn|def|function|func|class|struct|enum|trait|interface|type)\s+\w+",
    )
    .expect("valid signature regex")
});

pub(super) fn render(path: &Path, content: &str, options: ReadOptions) -> String {
    let lang = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(Language::from_extension)
        .unwrap_or(Language::Unknown);

    let mut filtered = minimal_filter(content, lang);
    if filtered.trim().is_empty() && !content.trim().is_empty() {
        filtered = content.to_string();
    }
    filtered = apply_line_window(&filtered, options.max_lines, options.tail_lines);

    if options.line_numbers {
        format_with_line_numbers(&filtered)
    } else {
        filtered
    }
}

impl Language {
    fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "rs" => Language::Rust,
            "py" | "pyw" => Language::Python,
            "js" | "mjs" | "cjs" => Language::JavaScript,
            "ts" | "tsx" => Language::TypeScript,
            "go" => Language::Go,
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" => Language::Cpp,
            "java" => Language::Java,
            "rb" => Language::Ruby,
            "sh" | "bash" | "zsh" => Language::Shell,
            "json" | "jsonc" | "json5" | "yaml" | "yml" | "toml" | "xml" | "csv" | "tsv"
            | "graphql" | "gql" | "sql" | "md" | "markdown" | "txt" | "env" | "lock" => {
                Language::Data
            }
            _ => Language::Unknown,
        }
    }

    fn comment_patterns(self) -> CommentPatterns {
        match self {
            Language::Rust => CommentPatterns {
                line: Some("//"),
                block_start: Some("/*"),
                block_end: Some("*/"),
                doc_line: Some("///"),
                doc_block_start: Some("/**"),
            },
            Language::Python => CommentPatterns {
                line: Some("#"),
                block_start: Some("\"\"\""),
                block_end: Some("\"\"\""),
                doc_line: None,
                doc_block_start: Some("\"\"\""),
            },
            Language::JavaScript
            | Language::TypeScript
            | Language::Go
            | Language::C
            | Language::Cpp
            | Language::Java => CommentPatterns {
                line: Some("//"),
                block_start: Some("/*"),
                block_end: Some("*/"),
                doc_line: None,
                doc_block_start: Some("/**"),
            },
            Language::Ruby => CommentPatterns {
                line: Some("#"),
                block_start: Some("=begin"),
                block_end: Some("=end"),
                doc_line: None,
                doc_block_start: None,
            },
            Language::Shell => CommentPatterns {
                line: Some("#"),
                block_start: None,
                block_end: None,
                doc_line: None,
                doc_block_start: None,
            },
            Language::Data => CommentPatterns {
                line: None,
                block_start: None,
                block_end: None,
                doc_line: None,
                doc_block_start: None,
            },
            Language::Unknown => CommentPatterns {
                line: Some("//"),
                block_start: Some("/*"),
                block_end: Some("*/"),
                doc_line: None,
                doc_block_start: None,
            },
        }
    }
}

fn minimal_filter(content: &str, lang: Language) -> String {
    let patterns = lang.comment_patterns();
    let mut result = String::with_capacity(content.len());
    let mut in_block_comment = false;
    let mut in_docstring = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if let (Some(start), Some(end)) = (patterns.block_start, patterns.block_end) {
            if !in_docstring
                && trimmed.contains(start)
                && !trimmed.starts_with(patterns.doc_block_start.unwrap_or("###"))
            {
                in_block_comment = true;
            }
            if in_block_comment {
                if trimmed.contains(end) {
                    in_block_comment = false;
                }
                continue;
            }
        }

        if lang == Language::Python && trimmed.starts_with("\"\"\"") {
            in_docstring = !in_docstring;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if in_docstring {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if let Some(line_comment) = patterns.line
            && trimmed.starts_with(line_comment)
        {
            if let Some(doc) = patterns.doc_line
                && trimmed.starts_with(doc)
            {
                result.push_str(line);
                result.push('\n');
            }
            continue;
        }

        if trimmed.is_empty() {
            result.push('\n');
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }

    MULTIPLE_BLANK_LINES
        .replace_all(&result, "\n\n")
        .trim()
        .to_string()
}

fn apply_line_window(content: &str, max_lines: Option<usize>, tail_lines: Option<usize>) -> String {
    if let Some(tail) = tail_lines {
        if tail == 0 {
            return String::new();
        }
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(tail);
        let mut result = lines[start..].join("\n");
        if content.ends_with('\n') {
            result.push('\n');
        }
        return result;
    }

    if let Some(max) = max_lines {
        return smart_truncate(content, max);
    }

    content.to_string()
}

fn smart_truncate(content: &str, max_lines: usize) -> String {
    if max_lines == 0 {
        return String::new();
    }

    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= max_lines {
        return content.to_string();
    }

    let mut result = Vec::with_capacity(max_lines + 1);
    let mut kept_lines = 0usize;

    for line in &lines {
        let trimmed = line.trim();
        let is_important = FUNC_SIGNATURE.is_match(trimmed)
            || IMPORT_PATTERN.is_match(trimmed)
            || trimmed.starts_with("pub ")
            || trimmed.starts_with("export ")
            || trimmed == "}"
            || trimmed == "{";

        if is_important || kept_lines < max_lines / 2 {
            result.push((*line).to_string());
            kept_lines += 1;
        }

        if kept_lines >= max_lines - 1 {
            break;
        }
    }

    result.push(format!("[{} more lines]", lines.len() - kept_lines));
    result.join("\n")
}

fn format_with_line_numbers(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let width = lines.len().to_string().len();
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        out.push_str(&format!("{:>width$} │ {}\n", i + 1, line, width = width));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn minimal_filter_removes_rust_comments_and_blank_noise() {
        let input = "// comment\n\n\nfn main() {\n    println!(\"hi\");\n}\n";
        let output = render(Path::new("main.rs"), input, ReadOptions::minimal());
        assert_eq!(output, "fn main() {\n    println!(\"hi\");\n}");
    }

    #[test]
    fn minimal_filter_preserves_data_content_except_blank_normalization() {
        let input = "{\n  \"glob\": \"packages/*\"\n}\n\n\n";
        let output = render(Path::new("package.json"), input, ReadOptions::minimal());
        assert_eq!(output, "{\n  \"glob\": \"packages/*\"\n}");
    }

    #[test]
    fn minimal_filter_removes_ruby_block_comments() {
        let input = "=begin\ncomment\n=end\nputs \"hi\"\n";
        let output = render(Path::new("script.rb"), input, ReadOptions::minimal());
        assert_eq!(output, "puts \"hi\"");
    }

    #[test]
    fn line_numbers_are_added_after_filtering() {
        let input = "// comment\nfn main() {}\n";
        let output = render(
            Path::new("main.rs"),
            input,
            ReadOptions {
                line_numbers: true,
                ..ReadOptions::minimal()
            },
        );
        assert_eq!(output, "1 │ fn main() {}\n");
    }

    #[test]
    fn smart_truncate_matches_rtk_marker_style() {
        let input = "line1\nline2\nline3\nline4\n";
        let output = render(
            Path::new("note.txt"),
            input,
            ReadOptions {
                max_lines: Some(3),
                ..ReadOptions::minimal()
            },
        );
        assert_eq!(output, "line1\n[3 more lines]");
    }

    #[test]
    fn smart_truncate_zero_lines_is_empty() {
        let output = render(
            Path::new("note.txt"),
            "line1\nline2\n",
            ReadOptions {
                max_lines: Some(0),
                ..ReadOptions::minimal()
            },
        );
        assert_eq!(output, "");
    }

    #[test]
    fn tail_window_keeps_last_lines_after_filtering() {
        let input = "// hidden\none\ntwo\nthree\n";
        let output = render(
            Path::new("main.rs"),
            input,
            ReadOptions {
                tail_lines: Some(2),
                ..ReadOptions::minimal()
            },
        );
        assert_eq!(output, "two\nthree");
    }
}
