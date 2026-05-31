//! `ls` — list a directory's entries, alphabetically, dirs suffixed with `/`.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::support::paths::resolve_in_cwd;
use super::support::truncate::{TRUNCATION_MARKER, cap_head};
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_opt_str, arg_opt_u64};

const DEFAULT_LIMIT: usize = 500;

pub struct LsTool;

impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "List directory contents. Entries are sorted alphabetically, with a \
         trailing '/' for directories, and include dotfiles. Output is \
         truncated to 500 entries."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list (default: current directory)" },
                "limit": { "type": "integer", "description": "Maximum number of entries to return (default: 500)" }
            }
        })
    }

    fn execute(
        &self,
        args: &Value,
        cwd: &Path,
        _cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError> {
        let path = arg_opt_str(args, "path").unwrap_or(".");
        let resolved = resolve_in_cwd(cwd, path)?;
        let limit = arg_opt_u64(args, "limit")
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_LIMIT);

        let read = fs::read_dir(&resolved)
            .map_err(|error| ToolError::new(format!("could not list {path}: {error}")))?;

        let mut entries: Vec<String> = Vec::new();
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            entries.push(if is_dir { format!("{name}/") } else { name });
        }
        entries.sort();

        let truncated = entries.len() > limit;
        entries.truncate(limit);

        let mut out = cap_head(&entries.join("\n"));
        if truncated && !out.ends_with(TRUNCATION_MARKER) {
            out.push_str(TRUNCATION_MARKER);
        }
        Ok(ToolOutput::new(out))
    }
}
