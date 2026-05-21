//! Model-visible function schemas and the static access policy for each
//! agent scope.

use serde_json::{Value, json};

pub const SPAWN_SUBAGENT_TOOL: &str = "spawn_subagent";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAccess {
    Full,
    ReadOnly,
}

impl ToolAccess {
    pub fn allows(self, name: &str) -> bool {
        match self {
            ToolAccess::Full => matches!(
                name,
                "read_file"
                    | "list_files"
                    | "bash"
                    | "edit_file"
                    | "apply_patch"
                    | "code_search"
                    | SPAWN_SUBAGENT_TOOL
            ),
            ToolAccess::ReadOnly => matches!(name, "read_file" | "list_files" | "code_search"),
        }
    }
}

pub(crate) fn tool_definitions(access: ToolAccess, include_subagents: bool) -> Vec<Value> {
    // These primitives mirror the workshop article, with `apply_patch` as
    // the reviewable multi-file editing path learned from sibling agent
    // projects. Together they let the model inspect code, find code,
    // change code, and verify with commands.
    let mut definitions = vec![
        json!({
            "type": "function",
            "name": "read_file",
            "description": "Read the contents of a relative file path. Do not use this with directories. Pass `offset` (1-indexed line number) and/or `limit` (line count) to read a slice instead of the whole file; truncated reads end with a notice that names the next offset and remaining-line count.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-indexed line number to start reading from."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Maximum number of lines to return."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "list_files",
            "description": "List files and directories at a relative path. Use '.' for the current directory.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "bash",
            "description": "Execute a shell command and return stdout/stderr. Use for builds, tests, and small checks.",
            "parameters": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "edit_file",
            "description": "Create a file when old_str is empty, or replace one exact old_str occurrence with new_str.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_str": { "type": "string" },
                    "new_str": { "type": "string" }
                },
                "required": ["path", "old_str", "new_str"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "apply_patch",
            "description": "Apply a reviewable patch. Use Codex patch format: *** Begin Patch; file sections with *** Add File, *** Update File, optional *** Move to, or *** Delete File; + added lines, - removed lines, space context lines; then *** End Patch.",
            "parameters": {
                "type": "object",
                "properties": {
                    "patch": { "type": "string" }
                },
                "required": ["patch"],
                "additionalProperties": false
            }
        }),
        json!({
            "type": "function",
            "name": "code_search",
            "description": "Search source text for a pattern, like ripgrep.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern", "path"],
                "additionalProperties": false
            }
        }),
    ];

    definitions.retain(|definition| {
        definition
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| access.allows(name))
    });

    if include_subagents && access.allows(SPAWN_SUBAGENT_TOOL) {
        definitions.push(json!({
            "type": "function",
            "name": SPAWN_SUBAGENT_TOOL,
            "description": "Run a focused helper agent with its own short context for bounded codebase exploration or review. The helper cannot edit files, run shell commands, or spawn more agents; it returns a concise summary for you to integrate.",
            "parameters": {
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The focused task for the helper agent."
                    },
                    "label": {
                        "type": "string",
                        "description": "Optional short human-readable label for the helper."
                    }
                },
                "required": ["task"],
                "additionalProperties": false
            }
        }));
    }

    definitions
}
