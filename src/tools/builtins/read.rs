//! `read` — read a text file, with optional line offset/limit.

use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::support::paths::resolve_in_cwd;
use super::support::truncate::{MAX_BYTES, MAX_LINES};
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_opt_u64, arg_str};

pub struct ReadTool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Debug, PartialEq, Eq)]
enum ReadTruncation {
    Complete {
        content: String,
    },
    Truncated {
        content: String,
        by: TruncatedBy,
        output_lines: usize,
    },
    FirstLineTooLarge,
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports text files and recognizes jpg, \
         png, gif, and webp images. Text output is truncated to 2000 lines or \
         50KB (whichever is hit first); use offset/limit for large files and \
         continue with offset until complete."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Read file contents")
    }

    fn prompt_guidelines(&self) -> &'static [&'static str] {
        &["Use read to examine files instead of cat or sed."]
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
                "offset": { "type": "integer", "description": "Line number to start reading from (1-indexed)" },
                "limit": { "type": "integer", "description": "Maximum number of lines to read" }
            },
            "required": ["path"]
        })
    }

    fn execute(
        &self,
        args: &Value,
        cwd: &Path,
        cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(ToolError::new("operation aborted"));
        }

        let path = arg_str(args, "path")?;
        let resolved = resolve_in_cwd(cwd, path)?;
        let bytes = fs::read(&resolved)
            .map_err(|error| ToolError::new(format!("could not read {path}: {error}")))?;

        if let Some(mime_type) = supported_image_mime(&bytes) {
            return Ok(ToolOutput::new(format!(
                "Read image file [{mime_type}]\n\
                 [Image omitted: nav tool results are currently text-only.]"
            )));
        }

        let content = String::from_utf8(bytes).map_err(|error| {
            ToolError::new(format!(
                "could not read {path} as UTF-8 text: {error}. Use bash for binary files."
            ))
        })?;

        let offset = arg_opt_u64(args, "offset").unwrap_or(1).max(1) as usize;
        let limit = arg_opt_u64(args, "limit").map(|value| value as usize);

        let all_lines: Vec<&str> = content.split('\n').collect();
        let start = offset - 1;
        if start >= all_lines.len() {
            return Err(ToolError::new(format!(
                "Offset {offset} is beyond end of file ({} lines total)",
                all_lines.len()
            )));
        }

        let (end, user_limited_lines) = match limit {
            Some(limit) => {
                let end = start.saturating_add(limit).min(all_lines.len());
                (end, Some(end - start))
            }
            None => (all_lines.len(), None),
        };

        let selected = all_lines[start..end].join("\n");
        let truncation = truncate_head_for_read(&selected);
        let start_line_display = start + 1;
        let output = match truncation {
            ReadTruncation::FirstLineTooLarge => {
                format_first_line_too_large(path, start_line_display, all_lines[start].len())
            }
            ReadTruncation::Truncated {
                content,
                by,
                output_lines,
            } => format_truncated_read(
                content,
                by,
                start_line_display,
                output_lines,
                all_lines.len(),
            ),
            ReadTruncation::Complete { content } => {
                format_complete_read(content, start, user_limited_lines, all_lines.len())
            }
        };

        Ok(ToolOutput::new(output))
    }
}

fn supported_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn truncate_head_for_read(content: &str) -> ReadTruncation {
    let lines = split_lines_for_counting(content);
    let total_bytes = content.len();

    if lines.len() <= MAX_LINES && total_bytes <= MAX_BYTES {
        return ReadTruncation::Complete {
            content: content.to_owned(),
        };
    }

    if lines
        .first()
        .is_some_and(|first_line| first_line.len() > MAX_BYTES)
    {
        return ReadTruncation::FirstLineTooLarge;
    }

    let mut output_lines = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (index, line) in lines.iter().take(MAX_LINES).enumerate() {
        let line_bytes = line.len() + usize::from(index > 0);
        if output_bytes + line_bytes > MAX_BYTES {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines.push(*line);
        output_bytes += line_bytes;
    }

    if output_lines.len() >= MAX_LINES && output_bytes <= MAX_BYTES {
        truncated_by = TruncatedBy::Lines;
    }

    ReadTruncation::Truncated {
        content: output_lines.join("\n"),
        by: truncated_by,
        output_lines: output_lines.len(),
    }
}

fn format_first_line_too_large(path: &str, line_number: usize, line_size: usize) -> String {
    format!(
        "[Line {line_number} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
        format_size(line_size),
        format_size(MAX_BYTES),
        line_number,
        shell_quote(path),
        MAX_BYTES
    )
}

fn format_truncated_read(
    content: String,
    truncated_by: TruncatedBy,
    start_line: usize,
    output_lines: usize,
    total_lines: usize,
) -> String {
    let end_line = start_line + output_lines.saturating_sub(1);
    let next_offset = end_line + 1;
    let note = match truncated_by {
        TruncatedBy::Lines => {
            format!(
                "[Showing lines {start_line}-{end_line} of {total_lines}. Use offset={next_offset} to continue.]"
            )
        }
        TruncatedBy::Bytes => {
            format!(
                "[Showing lines {start_line}-{end_line} of {total_lines} ({} limit). Use offset={next_offset} to continue.]",
                format_size(MAX_BYTES)
            )
        }
    };
    format!("{content}\n\n{note}")
}

fn format_complete_read(
    content: String,
    start: usize,
    user_limited_lines: Option<usize>,
    total_lines: usize,
) -> String {
    let Some(user_limited_lines) = user_limited_lines else {
        return content;
    };

    if start + user_limited_lines >= total_lines {
        return content;
    }

    let remaining = total_lines - (start + user_limited_lines);
    let next_offset = start + user_limited_lines + 1;
    format!("{content}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]")
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }

    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
