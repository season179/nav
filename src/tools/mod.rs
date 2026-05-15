mod fs;
mod shell;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn tool_definitions() -> Vec<Value> {
    // these five primitives mirror the workshop article. Together they let
    // the model inspect code, find code, change code, and verify with commands.
    vec![
        json!({
            "type": "function",
            "name": "read_file",
            "description": "Read the contents of a relative file path. Do not use this with directories.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
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
    ]
}

pub(super) async fn run_tool(
    cwd: &Path,
    timeout_secs: u64,
    name: &str,
    input: Value,
) -> Result<String> {
    // central dispatch keeps the trust boundary obvious. The model asks;
    // this Rust match decides exactly which local capability is allowed.
    match name {
        "read_file" => fs::read_file(cwd, string_arg(&input, "path")?),
        "list_files" => fs::list_files(cwd, string_arg(&input, "path")?),
        "bash" => shell::bash(cwd, timeout_secs, string_arg(&input, "command")?).await,
        "edit_file" => fs::edit_file(
            cwd,
            string_arg(&input, "path")?,
            string_arg(&input, "old_str")?,
            string_arg(&input, "new_str")?,
        ),
        "code_search" => {
            fs::code_search(
                cwd,
                string_arg(&input, "pattern")?,
                string_arg(&input, "path")?,
            )
            .await
        }
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

fn string_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string input field `{key}`"))
}
