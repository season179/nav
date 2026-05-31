//! `write` — create or overwrite a file, making parent directories as needed.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::support::paths::resolve_in_cwd;
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_str};

pub struct WriteTool;

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist, \
         overwrites if it does, and creates parent directories. Use for new \
         files or complete rewrites; use edit for targeted changes."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
                "content": { "type": "string", "description": "Content to write to the file" }
            },
            "required": ["path", "content"]
        })
    }

    fn execute(
        &self,
        args: &Value,
        cwd: &Path,
        _cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError> {
        let path = arg_str(args, "path")?;
        let content = arg_str(args, "content")?;
        let resolved = resolve_in_cwd(cwd, path)?;

        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ToolError::new(format!("could not create directories for {path}: {error}"))
            })?;
        }
        fs::write(&resolved, content)
            .map_err(|error| ToolError::new(format!("could not write {path}: {error}")))?;

        Ok(ToolOutput::new(format!(
            "Wrote {} byte(s) to {path}",
            content.len()
        )))
    }
}
