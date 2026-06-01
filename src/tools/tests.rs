//! Tool behavior tests, exercised through the `Registry` against a temp dir.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::model::ToolCall;

use super::{CancelFlag, Registry, ToolResult};

/// A throwaway workspace directory, removed on drop.
struct Workspace {
    path: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nav_tools_{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&path).expect("create workspace");
        Self { path }
    }

    fn write(&self, relative: &str, content: &str) {
        let target = self.path.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(target, content).expect("seed file");
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.path.join(relative)).expect("read file")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn no_cancel() -> CancelFlag {
    Arc::new(AtomicBool::new(false))
}

fn tool_call(tool: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "call-1".to_owned(),
        name: tool.to_owned(),
        arguments: args.to_string(),
    }
}

/// Execute a tool call through the registry and require success.
fn run(workspace: &Workspace, tool: &str, args: serde_json::Value) -> ToolResult {
    let registry = Registry::coding();
    let result = registry.execute_call(&tool_call(tool, args), &workspace.path, &no_cancel());
    assert!(!result.is_error, "{tool} failed: {}", result.content);
    result
}

fn run_error(workspace: &Workspace, tool: &str, args: serde_json::Value) -> ToolResult {
    let registry = Registry::coding();
    let result = registry.execute_call(&tool_call(tool, args), &workspace.path, &no_cancel());
    assert!(result.is_error, "{tool} should have failed");
    result
}

fn full_output_path(content: &str) -> &str {
    content
        .split("Full output: ")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("full output path")
}

#[test]
fn registry_advertises_the_seven_coding_tools() {
    let names: Vec<String> = Registry::coding()
        .defs()
        .into_iter()
        .map(|def| def.name)
        .collect();
    assert_eq!(
        names,
        ["read", "bash", "edit", "write", "grep", "find", "ls"]
    );
}

#[test]
fn read_returns_file_contents_and_honors_offset_limit() {
    let workspace = Workspace::new();
    workspace.write("notes.txt", "one\ntwo\nthree\nfour");

    let full = run(&workspace, "read", json!({ "path": "notes.txt" }));
    assert_eq!(full.content, "one\ntwo\nthree\nfour");

    let middle = run(
        &workspace,
        "read",
        json!({ "path": "notes.txt", "offset": 2, "limit": 2 }),
    );
    assert_eq!(middle.content, "two\nthree");
}

#[test]
fn write_creates_files_and_parent_directories() {
    let workspace = Workspace::new();
    run(
        &workspace,
        "write",
        json!({ "path": "nested/dir/out.txt", "content": "hello" }),
    );
    assert_eq!(workspace.read("nested/dir/out.txt"), "hello");
}

#[test]
fn edit_replaces_unique_text() {
    let workspace = Workspace::new();
    workspace.write("code.rs", "let a = 1;\nlet b = 2;\n");

    run(
        &workspace,
        "edit",
        json!({
            "path": "code.rs",
            "edits": [
                { "oldText": "let a = 1;", "newText": "let a = 10;" },
                { "oldText": "let b = 2;", "newText": "let b = 20;" }
            ]
        }),
    );
    assert_eq!(workspace.read("code.rs"), "let a = 10;\nlet b = 20;\n");
}

#[test]
fn edit_rejects_a_non_unique_match() {
    let workspace = Workspace::new();
    workspace.write("dup.txt", "x\nx\n");

    let error = run_error(
        &workspace,
        "edit",
        json!({ "path": "dup.txt", "edits": [{ "oldText": "x", "newText": "y" }] }),
    );
    assert!(error.content.contains("not unique"), "{}", error.content);
    // The file is left untouched on failure.
    assert_eq!(workspace.read("dup.txt"), "x\nx\n");
}

#[test]
fn ls_lists_entries_sorted_with_directory_slashes() {
    let workspace = Workspace::new();
    workspace.write("b.txt", "");
    workspace.write("a/inner.txt", "");

    let output = run(&workspace, "ls", json!({}));
    assert_eq!(output.content, "a/\nb.txt");
}

#[test]
fn find_matches_a_glob_across_directories() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "");
    workspace.write("src/lib.rs", "");
    workspace.write("README.md", "");

    let output = run(&workspace, "find", json!({ "pattern": "**/*.rs" }));
    assert_eq!(output.content, "src/lib.rs\nsrc/main.rs");
}

#[test]
fn grep_reports_matches_with_path_and_line_number() {
    let workspace = Workspace::new();
    workspace.write("a.txt", "alpha\nBETA\ngamma");

    let literal = run(
        &workspace,
        "grep",
        json!({ "pattern": "alpha", "literal": true }),
    );
    assert_eq!(literal.content.trim(), "a.txt:1:alpha");

    let case_insensitive = run(
        &workspace,
        "grep",
        json!({ "pattern": "beta", "ignoreCase": true }),
    );
    assert_eq!(case_insensitive.content.trim(), "a.txt:2:BETA");
}

#[test]
fn bash_runs_a_command_in_the_workspace() {
    let workspace = Workspace::new();
    workspace.write("marker.txt", "");

    let output = run(&workspace, "bash", json!({ "command": "ls" }));
    assert!(output.content.contains("marker.txt"), "{}", output.content);
}

#[test]
fn bash_reports_a_nonzero_exit_status() {
    let workspace = Workspace::new();
    let output = run(&workspace, "bash", json!({ "command": "exit 3" }));
    assert!(output.content.contains("status 3"), "{}", output.content);
}

#[test]
fn bash_interleaves_stdout_and_stderr_in_arrival_order() {
    let workspace = Workspace::new();
    let output = run(
        &workspace,
        "bash",
        json!({
            "command": "printf 'out1\\n'; sleep 0.1; printf 'err1\\n' >&2; sleep 0.1; printf 'out2\\n'"
        }),
    );

    let out1 = output.content.find("out1").expect("stdout chunk");
    let err1 = output.content.find("err1").expect("stderr chunk");
    let out2 = output.content.find("out2").expect("second stdout chunk");
    assert!(
        out1 < err1 && err1 < out2,
        "output should preserve chunk order: {}",
        output.content
    );
}

#[test]
fn bash_uses_bash_when_available() {
    if !std::path::Path::new("/bin/bash").exists() {
        return;
    }

    let workspace = Workspace::new();
    let output = run(
        &workspace,
        "bash",
        json!({ "command": "echo ${BASH_VERSION}" }),
    );

    assert!(
        !output.content.trim().is_empty(),
        "bash-specific variable should be set: {}",
        output.content
    );
}

#[test]
fn bash_sanitizes_ansi_and_binary_output() {
    let workspace = Workspace::new();
    let output = run(
        &workspace,
        "bash",
        json!({ "command": "printf '\\033[31mred\\033[0m\\000blue\\001\\n'" }),
    );

    assert!(output.content.contains("redblue"), "{}", output.content);
    assert!(
        !output.content.contains('\u{1b}') && !output.content.contains('\0'),
        "control characters should be stripped: {:?}",
        output.content
    );
}

#[test]
fn bash_saves_full_output_when_truncated() {
    let workspace = Workspace::new();
    let output = run(
        &workspace,
        "bash",
        json!({
            "command": "i=0; while [ $i -le 2050 ]; do echo line-$i; i=$((i + 1)); done"
        }),
    );

    assert!(
        output.content.contains("Full output:"),
        "truncated output should point to the full log: {}",
        output.content
    );
    assert!(output.content.contains("line-2050"), "{}", output.content);
    assert!(!output.content.contains("line-0\n"), "{}", output.content);

    let path = full_output_path(&output.content);
    let full = fs::read_to_string(path).expect("read full output");
    let _ = fs::remove_file(path);
    assert!(full.contains("line-0\n"), "{}", full);
    assert!(full.contains("line-2050"), "{}", full);
}

#[cfg(unix)]
#[test]
fn bash_timeout_kills_background_descendants() {
    let workspace = Workspace::new();
    let marker = workspace.path.join("timeout_descendant_survived");
    let output = run(
        &workspace,
        "bash",
        json!({
            "command": "(sleep 2; touch timeout_descendant_survived) & wait",
            "timeout": 1
        }),
    );

    assert!(output.content.contains("timed out"), "{}", output.content);
    thread::sleep(Duration::from_secs(3));
    assert!(
        !marker.exists(),
        "timeout should kill the whole process group, not just the shell"
    );
}

#[cfg(unix)]
#[test]
fn bash_cancel_kills_background_descendants() {
    let workspace = Workspace::new();
    let marker = workspace.path.join("cancel_descendant_survived");
    let cancel = no_cancel();
    let cancel_for_thread = Arc::clone(&cancel);

    let runner = {
        let path = workspace.path.clone();
        thread::spawn(move || {
            let registry = Registry::coding();
            registry.execute_call(
                &tool_call(
                    "bash",
                    json!({ "command": "(sleep 2; touch cancel_descendant_survived) & wait" }),
                ),
                &path,
                &cancel_for_thread,
            )
        })
    };

    thread::sleep(Duration::from_millis(150));
    cancel.store(true, Ordering::Relaxed);
    let output = runner.join().expect("bash runner");

    assert!(output.content.contains("cancelled"), "{}", output.content);
    thread::sleep(Duration::from_secs(3));
    assert!(
        !marker.exists(),
        "cancel should kill the whole process group, not just the shell"
    );
}

#[test]
fn path_tools_refuse_to_escape_the_workspace() {
    let workspace = Workspace::new();
    let error = run_error(&workspace, "read", json!({ "path": "../../etc/passwd" }));
    assert!(
        error.content.contains("escapes the workspace"),
        "{}",
        error.content
    );
}

#[test]
fn registry_reports_unknown_tools_as_error_results() {
    let workspace = Workspace::new();
    let result = Registry::coding().execute_call(
        &tool_call("nope", json!({})),
        &workspace.path,
        &no_cancel(),
    );

    assert!(result.is_error);
    assert_eq!(result.content, "unknown tool: nope");
}

#[test]
fn registry_reports_invalid_json_as_error_results() {
    let workspace = Workspace::new();
    let result = Registry::coding().execute_call(
        &ToolCall {
            id: "call-1".to_owned(),
            name: "ls".to_owned(),
            arguments: "{".to_owned(),
        },
        &workspace.path,
        &no_cancel(),
    );

    assert!(result.is_error);
    assert!(
        result.content.contains("invalid tool arguments"),
        "{}",
        result.content
    );
}
