//! `find` — list files matching a glob, relative to the search directory.

use std::path::Path;

use serde_json::{Value, json};

use super::support::glob::glob_to_regex;
use super::support::paths::{display_relative, resolve_in_cwd};
use super::support::truncate::{TRUNCATION_MARKER, cap_head};
use super::support::walk::walk_files;
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_opt_str, arg_opt_u64, arg_str};

const DEFAULT_LIMIT: usize = 1000;

pub struct FindTool;

impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Find files by glob pattern (e.g. '*.rs', '**/*.json', 'src/**/*.rs'). \
         Returns paths relative to the search directory. Skips .git and common \
         vendor directories. Output is truncated to 1000 results."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Find files by glob pattern (skips .git and common vendor directories)")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern to match files" },
                "path": { "type": "string", "description": "Directory to search in (default: current directory)" },
                "limit": { "type": "integer", "description": "Maximum number of results (default: 1000)" }
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

        let matcher = glob_to_regex(pattern)?;
        let mut results: Vec<String> = walk_files(&root, cancel)?
            .into_iter()
            .map(|path| display_relative(&root, &path))
            .filter(|relative| matcher.is_match(relative))
            .collect();
        results.sort();

        if results.is_empty() {
            return Ok(ToolOutput::new("No files matched."));
        }

        let truncated = results.len() > limit;
        results.truncate(limit);

        let mut out = cap_head(&results.join("\n"));
        if truncated && !out.ends_with(TRUNCATION_MARKER) {
            out.push_str(TRUNCATION_MARKER);
        }
        Ok(ToolOutput::new(out))
    }
}
