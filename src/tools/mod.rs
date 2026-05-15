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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    // ── tool_definitions ──────────────────────────────────────────

    #[test]
    fn tool_definitions_returns_all_five_tools() {
        let defs = tool_definitions();
        assert_eq!(defs.len(), 5);
        let names: Vec<&str> = defs
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .collect();
        for expected in ["read_file", "list_files", "bash", "edit_file", "code_search"] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn tool_definitions_have_valid_schemas() {
        for def in tool_definitions() {
            assert_eq!(def["type"], "function");
            let params = &def["parameters"];
            assert_eq!(params["type"], "object");
            assert!(params["properties"].is_object());
            assert!(params["required"].is_array());
        }
    }

    // ── run_tool dispatch ─────────────────────────────────────────

    #[tokio::test]
    async fn run_tool_rejects_unknown_tool() {
        let cwd = Path::new("/tmp");
        let err = run_tool(cwd, 5, "fly_away", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("unknown tool: fly_away"));
    }

    #[tokio::test]
    async fn run_tool_read_file_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("hello.txt"), "world").unwrap();

        let result = run_tool(&cwd, 5, "read_file", json!({"path": "hello.txt"}))
            .await
            .unwrap();
        assert_eq!(result, "world");
    }

    #[tokio::test]
    async fn run_tool_list_files_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("a.txt"), "").unwrap();
        fs::create_dir(cwd.join("subdir")).unwrap();

        let result = run_tool(&cwd, 5, "list_files", json!({"path": "."}))
            .await
            .unwrap();
        let parsed: Vec<String> = serde_json::from_str(&result).unwrap();
        assert!(parsed.contains(&"a.txt".to_string()));
        assert!(parsed.contains(&"subdir/".to_string()));
    }

    #[tokio::test]
    async fn run_tool_bash_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();

        let result = run_tool(&cwd, 5, "bash", json!({"command": "echo ok"}))
            .await
            .unwrap();
        assert!(result.contains("ok"));
    }

    #[tokio::test]
    async fn run_tool_edit_file_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("f.txt"), "hello world").unwrap();

        let result = run_tool(
            &cwd,
            5,
            "edit_file",
            json!({"path": "f.txt", "old_str": "world", "new_str": "nav"}),
        )
        .await
        .unwrap();
        assert!(result.contains("edited"));
        assert_eq!(fs::read_to_string(cwd.join("f.txt")).unwrap(), "hello nav");
    }

    // ── string_arg ────────────────────────────────────────────────

    #[test]
    fn string_arg_extracts_existing_field() {
        let input = json!({"path": "foo.rs"});
        assert_eq!(string_arg(&input, "path").unwrap(), "foo.rs");
    }

    #[test]
    fn string_arg_rejects_missing_field() {
        let input = json!({"path": "foo.rs"});
        let err = string_arg(&input, "command").unwrap_err();
        assert!(err.to_string().contains("missing string input field `command`"));
    }

    #[test]
    fn string_arg_rejects_non_string_field() {
        let input = json!({"path": 42});
        let err = string_arg(&input, "path").unwrap_err();
        assert!(err.to_string().contains("missing string input field `path`"));
    }
}
