//! `read` — read a text file, with optional line offset/limit.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::support::paths::resolve_in_cwd;
use super::support::truncate::cap_head;
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_opt_u64, arg_str};

pub struct ReadTool;

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read the contents of a text file. Output is truncated to 2000 lines or \
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
        _cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError> {
        let path = arg_str(args, "path")?;
        let resolved = resolve_in_cwd(cwd, path)?;
        let content = fs::read_to_string(&resolved)
            .map_err(|error| ToolError::new(format!("could not read {path}: {error}")))?;

        let offset = arg_opt_u64(args, "offset").unwrap_or(1).max(1) as usize;
        let limit = arg_opt_u64(args, "limit").map(|value| value as usize);

        let mut lines = content.lines().skip(offset - 1);
        let selected: Vec<&str> = match limit {
            Some(limit) => lines.by_ref().take(limit).collect(),
            None => lines.by_ref().collect(),
        };

        Ok(ToolOutput::new(cap_head(&selected.join("\n"))))
    }
}
