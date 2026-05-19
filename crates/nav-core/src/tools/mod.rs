mod fs;
mod patch;
mod shell;
mod truncate;

use crate::mutation::MutationResult;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::path::Path;

use crate::skills::Catalog;
use truncate::{TruncateMode, bound};

// `bash` errors tend to appear at the tail (assert failures, panics, traceback
// footers), so it gets head+tail. `read_file` / `code_search` are head-only
// because the earliest matches/lines are the most useful.
const BASH_HEAD_LINES: usize = 200;

pub(super) fn tool_definitions() -> Vec<Value> {
    // These primitives mirror the workshop article, with `apply_patch` as the
    // reviewable multi-file editing path learned from sibling agent projects.
    // Together they let the model inspect code, find code, change code, and
    // verify with commands.
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
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub output: String,
    pub mutation: Option<MutationResult>,
}

impl ToolResult {
    fn text(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            mutation: None,
        }
    }

    pub(super) fn mutation(output: impl Into<String>, mutation: MutationResult) -> Self {
        Self {
            output: output.into(),
            mutation: Some(mutation),
        }
    }
}

pub async fn run_tool(
    cwd: &Path,
    skills: &Catalog,
    timeout_secs: u64,
    name: &str,
    input: Value,
) -> Result<ToolResult> {
    // central dispatch keeps the trust boundary obvious. The model asks;
    // this Rust match decides exactly which local capability is allowed.
    // Skill directories are accepted as extra read roots; mutating tools stay
    // workspace-only.
    let skill_dirs = skills.skill_dirs();
    match name {
        "read_file" => fs::read_file(cwd, skill_dirs, string_arg(&input, "path")?)
            .map(|out| ToolResult::text(bound(out, TruncateMode::Head))),
        "list_files" => fs::list_files(cwd, skill_dirs, string_arg(&input, "path")?)
            .map(ToolResult::text),
        "bash" => shell::bash(cwd, timeout_secs, string_arg(&input, "command")?)
            .await
            .map(|out| {
                ToolResult::text(
                    bound(
                        out,
                        TruncateMode::HeadTail {
                            head_lines: BASH_HEAD_LINES,
                        },
                    ),
                )
            }),
        "edit_file" => fs::edit_file_with_metadata(
            cwd,
            string_arg(&input, "path")?,
            string_arg(&input, "old_str")?,
            string_arg(&input, "new_str")?,
        ),
        "apply_patch" => patch::apply_patch(cwd, string_arg(&input, "patch")?),
        "code_search" => fs::code_search(
            cwd,
            skill_dirs,
            string_arg(&input, "pattern")?,
            string_arg(&input, "path")?,
        )
        .await
        .map(|out| ToolResult::text(bound(out, TruncateMode::Head))),
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

fn string_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string input field `{key}`"))
}

pub fn failed_mutation_summary(name: &str, input: &Value) -> Option<String> {
    let paths = match name {
        "edit_file" => string_arg(input, "path")
            .ok()
            .map(|path| vec![path.to_string()])?,
        "apply_patch" => {
            let patch = string_arg(input, "patch").ok()?;
            let paths = patch::target_paths_from_patch(patch);
            if paths.is_empty() {
                return Some("failed to apply patch".to_string());
            }
            paths
        }
        _ => return None,
    };
    Some(format!("failed to mutate {}", paths.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::tempdir;

    // ── tool_definitions ──────────────────────────────────────────

    #[test]
    fn tool_definitions_returns_all_six_tools() {
        let defs = tool_definitions();
        assert_eq!(defs.len(), 6);
        let names: Vec<&str> = defs
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .collect();
        for expected in [
            "read_file",
            "list_files",
            "bash",
            "edit_file",
            "apply_patch",
            "code_search",
        ] {
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
        let err = run_tool(cwd, &Catalog::default(), 5, "fly_away", json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown tool: fly_away"));
    }

    #[tokio::test]
    async fn run_tool_read_file_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("hello.txt"), "world").unwrap();

        let result = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            "read_file",
            json!({"path": "hello.txt"}),
        )
        .await
        .unwrap();
        assert_eq!(result.output, "world");
        assert!(result.mutation.is_none());
    }

    #[tokio::test]
    async fn run_tool_list_files_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("a.txt"), "").unwrap();
        fs::create_dir(cwd.join("subdir")).unwrap();

        let result = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            "list_files",
            json!({"path": "."}),
        )
        .await
        .unwrap();
        let parsed: Vec<String> = serde_json::from_str(&result.output).unwrap();
        assert!(parsed.contains(&"a.txt".to_string()));
        assert!(parsed.contains(&"subdir/".to_string()));
    }

    #[tokio::test]
    async fn run_tool_bash_output_is_bounded() {
        // A bash command that emits more than MAX_BYTES should come back
        // truncated with the marker, so it lands the same way in the prompt
        // and in the session log.
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();

        let result = run_tool(
            &cwd,
            &Catalog::default(),
            10,
            "bash",
            json!({"command": "yes hellohello | head -n 20000"}),
        )
        .await
        .unwrap();

        assert!(result.output.contains("[truncated"));
        assert!(
            result.output.len() < 80 * 1024,
            "result was {} bytes",
            result.output.len()
        );
        assert!(result.mutation.is_none());
    }

    #[tokio::test]
    async fn run_tool_bash_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();

        let result = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            "bash",
            json!({"command": "echo ok"}),
        )
        .await
        .unwrap();
        assert!(result.output.contains("ok"));
        assert!(result.mutation.is_none());
    }

    #[tokio::test]
    async fn run_tool_edit_file_dispatches() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("f.txt"), "hello world").unwrap();

        let result = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            "edit_file",
            json!({"path": "f.txt", "old_str": "world", "new_str": "nav"}),
        )
        .await
        .unwrap();
        assert!(result.output.contains("edited"));
        let mutation = result
            .mutation
            .expect("edit_file should report mutation metadata");
        assert_eq!(mutation.changes.len(), 1);
        assert_eq!(mutation.changes[0].path, "f.txt");
        assert!(mutation.changes[0].diff.contains("-hello world"));
        assert!(mutation.changes[0].diff.contains("+hello nav"));
        assert_eq!(fs::read_to_string(cwd.join("f.txt")).unwrap(), "hello nav");
    }

    #[tokio::test]
    async fn run_tool_apply_patch_dispatches_with_multi_file_mutation_metadata() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        fs::write(cwd.join("existing.txt"), "old\nline\n").unwrap();

        let result = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            "apply_patch",
            json!({
                "patch": "*** Begin Patch\n*** Update File: existing.txt\n@@\n-old\n+new\n line\n*** Add File: added.txt\n+hello\n*** End Patch\n"
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("updated 2 files"));
        let mutation = result
            .mutation
            .expect("apply_patch should report mutation metadata");
        assert_eq!(mutation.changes.len(), 2);
        assert_eq!(mutation.changes[0].path, "existing.txt");
        assert_eq!(mutation.changes[0].additions, 1);
        assert_eq!(mutation.changes[0].deletions, 1);
        assert_eq!(mutation.changes[0].line_start, Some(1));
        assert!(mutation.changes[0].diff.contains("-old"));
        assert!(mutation.changes[0].diff.contains("+new"));
        assert_eq!(mutation.changes[1].path, "added.txt");
        assert_eq!(mutation.changes[1].additions, 1);
        assert_eq!(
            fs::read_to_string(cwd.join("existing.txt")).unwrap(),
            "new\nline\n"
        );
        assert_eq!(
            fs::read_to_string(cwd.join("added.txt")).unwrap(),
            "hello\n"
        );
    }

    #[tokio::test]
    async fn run_tool_reads_skill_md_under_skill_dir() {
        use crate::skills::{Skill, SkillScope};
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let skill_home = tempdir().unwrap();
        let skill_dir = skill_home.path().canonicalize().unwrap().join("demo");
        fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        fs::write(
            &skill_md,
            "---\nname: demo\ndescription: d\n---\nSkill body\n",
        )
        .unwrap();
        let catalog = Catalog::new(vec![Skill {
            name: "demo".into(),
            description: "d".into(),
            skill_md_path: skill_md.clone(),
            skill_dir: skill_dir.clone(),
            scope: SkillScope::User,
        }]);

        let result = run_tool(
            &cwd,
            &catalog,
            5,
            "read_file",
            json!({"path": skill_md.to_string_lossy()}),
        )
        .await
        .unwrap();
        assert!(result.output.contains("Skill body"));
    }

    #[tokio::test]
    async fn run_tool_rejects_absolute_path_outside_skill_dirs() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let err = run_tool(
            &cwd,
            &Catalog::default(),
            5,
            "read_file",
            json!({"path": "/etc/hosts"}),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("absolute paths are only allowed"),
            "unexpected error: {err}"
        );
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
        assert!(
            err.to_string()
                .contains("missing string input field `command`")
        );
    }

    #[test]
    fn string_arg_rejects_non_string_field() {
        let input = json!({"path": 42});
        let err = string_arg(&input, "path").unwrap_err();
        assert!(
            err.to_string()
                .contains("missing string input field `path`")
        );
    }
}
