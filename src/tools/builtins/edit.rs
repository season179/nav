//! `edit` — exact text replacement. Each `oldText` must match exactly once in
//! the original file; edits are applied against the original (not incrementally)
//! and may not overlap.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::support::paths::resolve_in_cwd;
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_str};

pub struct EditTool;

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a file by exact text replacement. Every edits[].oldText must match \
         a unique, non-overlapping region of the original file. Apply multiple \
         disjoint changes in one call; keep each oldText minimal but unique."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements, each matched against the original file.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "Exact text to replace; must be unique in the file" },
                            "newText": { "type": "string", "description": "Replacement text" }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
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
        let original = fs::read_to_string(&resolved)
            .map_err(|error| ToolError::new(format!("could not read {path}: {error}")))?;

        let edits = args
            .get("edits")
            .and_then(Value::as_array)
            .ok_or_else(|| ToolError::new("edits must be an array"))?;
        if edits.is_empty() {
            return Err(ToolError::new("edits must not be empty"));
        }

        // Resolve every edit to a unique [start, end) span against the original.
        let mut spans: Vec<(usize, usize, &str)> = Vec::with_capacity(edits.len());
        for edit in edits {
            let old = edit
                .get("oldText")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::new("each edit needs a string oldText"))?;
            let new = edit
                .get("newText")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::new("each edit needs a string newText"))?;
            if old.is_empty() {
                return Err(ToolError::new("oldText must not be empty"));
            }

            let matches: Vec<usize> = original
                .match_indices(old)
                .map(|(index, _)| index)
                .collect();
            match matches.len() {
                0 => {
                    return Err(ToolError::new(format!(
                        "oldText not found in {path}: {old:?}"
                    )));
                }
                1 => spans.push((matches[0], matches[0] + old.len(), new)),
                count => {
                    return Err(ToolError::new(format!(
                        "oldText is not unique in {path} ({count} matches): {old:?}"
                    )));
                }
            }
        }

        // Apply left-to-right, rejecting overlaps.
        spans.sort_by_key(|(start, _, _)| *start);
        for pair in spans.windows(2) {
            if pair[0].1 > pair[1].0 {
                return Err(ToolError::new(
                    "edits overlap; merge nearby changes into one edit",
                ));
            }
        }

        let mut result = String::with_capacity(original.len());
        let mut cursor = 0;
        for (start, end, new) in &spans {
            result.push_str(&original[cursor..*start]);
            result.push_str(new);
            cursor = *end;
        }
        result.push_str(&original[cursor..]);

        fs::write(&resolved, &result)
            .map_err(|error| ToolError::new(format!("could not write {path}: {error}")))?;

        Ok(ToolOutput::new(format!(
            "Applied {} edit(s) to {path}",
            spans.len()
        )))
    }
}
