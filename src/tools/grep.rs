//! `grep` — search file contents for a regex (or literal) pattern.

use std::fs;
use std::path::Path;

use regex::RegexBuilder;
use serde_json::{Value, json};

use super::glob::glob_to_regex;
use super::paths::{display_relative, resolve_in_cwd};
use super::truncate::{TRUNCATION_MARKER, cap_head};
use super::walk::walk_files;
use super::{
    CancelFlag, Tool, ToolError, ToolOutput, arg_opt_bool, arg_opt_str, arg_opt_u64, arg_str,
};

const DEFAULT_LIMIT: usize = 100;
const MAX_LINE_LENGTH: usize = 1000;

pub struct GrepTool;

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents for a pattern. Returns matching lines with file \
         paths and line numbers. Skips .git and common vendor directories. \
         Output is truncated to 100 matches."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Search pattern (regex, or literal when literal=true)" },
                "path": { "type": "string", "description": "Directory or file to search (default: current directory)" },
                "glob": { "type": "string", "description": "Filter files by glob, e.g. '*.rs' or '**/*.spec.ts'" },
                "ignoreCase": { "type": "boolean", "description": "Case-insensitive search (default: false)" },
                "literal": { "type": "boolean", "description": "Treat pattern as a literal string instead of regex (default: false)" },
                "context": { "type": "integer", "description": "Lines of context to show before and after each match (default: 0)" },
                "limit": { "type": "integer", "description": "Maximum number of matches to return (default: 100)" }
            },
            "required": ["pattern"]
        })
    }

    fn execute(
        &self,
        args: &Value,
        cwd: &Path,
        cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError> {
        let pattern = arg_str(args, "pattern")?;
        let base = arg_opt_str(args, "path").unwrap_or(".");
        let root = resolve_in_cwd(cwd, base)?;
        let limit = arg_opt_u64(args, "limit")
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_LIMIT);
        let context = arg_opt_u64(args, "context")
            .map(|value| value as usize)
            .unwrap_or(0);

        let needle = if arg_opt_bool(args, "literal") {
            regex::escape(pattern)
        } else {
            pattern.to_owned()
        };
        let regex = RegexBuilder::new(&needle)
            .case_insensitive(arg_opt_bool(args, "ignoreCase"))
            .build()
            .map_err(|error| ToolError::new(format!("invalid pattern {pattern:?}: {error}")))?;

        // A single file searches just that file; a directory walks recursively,
        // optionally filtered by `glob`.
        let files = if root.is_file() {
            vec![root.clone()]
        } else {
            let glob = arg_opt_str(args, "glob").map(glob_to_regex).transpose()?;
            let mut files = walk_files(&root, cancel)?;
            if let Some(glob) = glob {
                files.retain(|path| glob.is_match(&display_relative(&root, path)));
            }
            files.sort();
            files
        };

        let mut out = String::new();
        let mut matches = 0;
        let mut truncated = false;

        'files: for path in &files {
            let Ok(content) = fs::read_to_string(path) else {
                continue; // skip binary / unreadable files
            };
            let lines: Vec<&str> = content.lines().collect();
            let label = display_relative(&root, path);

            for (index, line) in lines.iter().enumerate() {
                if !regex.is_match(line) {
                    continue;
                }
                if matches >= limit {
                    truncated = true;
                    break 'files;
                }
                matches += 1;

                let start = index.saturating_sub(context);
                let end = (index + context + 1).min(lines.len());
                for (offset, context_line) in lines[start..end].iter().enumerate() {
                    let line_number = start + offset + 1;
                    let separator = if start + offset == index { ':' } else { '-' };
                    out.push_str(&format!(
                        "{label}{separator}{line_number}{separator}{}\n",
                        clip(context_line)
                    ));
                }
            }
        }

        if matches == 0 {
            return Ok(ToolOutput::new("No matches."));
        }

        let mut capped = cap_head(&out);
        if truncated && !capped.ends_with(TRUNCATION_MARKER) {
            capped.push_str(TRUNCATION_MARKER);
        }
        Ok(ToolOutput::new(capped))
    }
}

/// Clip a single very long line so one match can't dominate the output.
fn clip(line: &str) -> String {
    if line.len() <= MAX_LINE_LENGTH {
        return line.to_owned();
    }
    let mut end = MAX_LINE_LENGTH;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}
