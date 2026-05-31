//! Tool behavior tests, exercised through the `Registry` against a temp dir.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::json;

use super::{CancelFlag, Registry, ToolOutput};

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

/// Run a tool by name and require success.
fn run(workspace: &Workspace, tool: &str, args: serde_json::Value) -> ToolOutput {
    let registry = Registry::coding();
    registry
        .get(tool)
        .unwrap_or_else(|| panic!("tool {tool} is registered"))
        .execute(&args, &workspace.path, &no_cancel())
        .unwrap_or_else(|error| panic!("{tool} failed: {}", error.message))
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

    let error = Registry::coding()
        .get("edit")
        .unwrap()
        .execute(
            &json!({ "path": "dup.txt", "edits": [{ "oldText": "x", "newText": "y" }] }),
            &workspace.path,
            &no_cancel(),
        )
        .expect_err("ambiguous match must fail");
    assert!(error.message.contains("not unique"), "{}", error.message);
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
fn path_tools_refuse_to_escape_the_workspace() {
    let workspace = Workspace::new();
    let error = Registry::coding()
        .get("read")
        .unwrap()
        .execute(
            &json!({ "path": "../../etc/passwd" }),
            &workspace.path,
            &no_cancel(),
        )
        .expect_err("escaping the workspace must fail");
    assert!(
        error.message.contains("escapes the workspace"),
        "{}",
        error.message
    );
}
