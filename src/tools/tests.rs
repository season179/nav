//! Tool behavior tests, exercised through the `Registry` against a temp dir.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::json;

use crate::model::ToolCall;

use super::{CancelFlag, Registry, ToolResult};

static ENV_LOCK: Mutex<()> = Mutex::new(());

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

fn has_rg() -> bool {
    Command::new("rg")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
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

struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }

    fn unset(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.original {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
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
    assert_eq!(
        middle.content,
        "two\nthree\n\n[1 more lines in file. Use offset=4 to continue.]"
    );
}

#[test]
fn read_rejects_offset_beyond_end_of_file() {
    let workspace = Workspace::new();
    workspace.write("notes.txt", "one\ntwo");

    let error = run_error(
        &workspace,
        "read",
        json!({ "path": "notes.txt", "offset": 3 }),
    );
    assert!(
        error
            .content
            .contains("Offset 3 is beyond end of file (2 lines total)"),
        "{}",
        error.content
    );
}

#[test]
fn read_reports_how_to_continue_when_output_is_truncated() {
    let workspace = Workspace::new();
    let content = (1..=2050)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    workspace.write("long.txt", &content);

    let output = run(&workspace, "read", json!({ "path": "long.txt" }));
    assert!(
        output.content.starts_with("line 1\nline 2\n"),
        "{}",
        output.content
    );
    assert!(
        output
            .content
            .ends_with("[Showing lines 1-2000 of 2050. Use offset=2001 to continue.]"),
        "{}",
        output.content
    );
}

#[test]
fn read_reports_first_line_that_exceeds_the_byte_limit() {
    let workspace = Workspace::new();
    workspace.write(
        "wide.txt",
        &"x".repeat(super::support::truncate::MAX_BYTES + 1),
    );

    let output = run(&workspace, "read", json!({ "path": "wide.txt" }));
    assert!(
        output
            .content
            .contains("[Line 1 is 50.0KB, exceeds 50.0KB limit."),
        "{}",
        output.content
    );
    assert!(
        output
            .content
            .contains("Use bash: sed -n '1p' 'wide.txt' | head -c 51200"),
        "{}",
        output.content
    );
}

#[test]
fn read_recognizes_supported_images_without_dumping_binary() {
    let workspace = Workspace::new();
    fs::write(
        workspace.path.join("image.png"),
        b"\x89PNG\r\n\x1a\nminimal test payload",
    )
    .expect("write image");

    let output = run(&workspace, "read", json!({ "path": "image.png" }));
    assert_eq!(
        output.content,
        "Read image file [image/png]\n[Image omitted: nav tool results are currently text-only.]"
    );
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
fn grep_respects_gitignore_when_ripgrep_is_available() {
    if !has_rg() {
        return;
    }

    let workspace = Workspace::new();
    fs::create_dir_all(workspace.path.join(".git")).expect("create git marker");
    workspace.write(".gitignore", "ignored.txt\n");
    workspace.write("ignored.txt", "needle ignored\n");
    workspace.write("visible.txt", "needle visible\n");

    let output = run(&workspace, "grep", json!({ "pattern": "needle" }));
    assert!(
        output.content.contains("visible.txt:1:needle visible"),
        "{}",
        output.content
    );
    assert!(
        !output.content.contains("ignored.txt"),
        "{}",
        output.content
    );
}

#[test]
fn grep_supports_output_modes_and_offset() {
    let workspace = Workspace::new();
    workspace.write("a.txt", "needle one\nneedle two\nneedle three\n");
    workspace.write("b.txt", "needle four\n");

    let paged = run(
        &workspace,
        "grep",
        json!({ "pattern": "needle", "path": "a.txt", "offset": 1, "limit": 1 }),
    );
    assert!(
        paged.content.contains("a.txt:2:needle two"),
        "{}",
        paged.content
    );
    assert!(
        !paged.content.contains("a.txt:1:needle one"),
        "{}",
        paged.content
    );

    let files = run(
        &workspace,
        "grep",
        json!({ "pattern": "needle", "outputMode": "files_with_matches" }),
    );
    assert!(
        files.content.lines().any(|line| line == "a.txt"),
        "{}",
        files.content
    );
    assert!(
        files.content.lines().any(|line| line == "b.txt"),
        "{}",
        files.content
    );

    let counts = run(
        &workspace,
        "grep",
        json!({ "pattern": "needle", "output_mode": "count" }),
    );
    assert!(counts.content.contains("a.txt:3"), "{}", counts.content);
    assert!(counts.content.contains("b.txt:1"), "{}", counts.content);
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
            "command": "printf 'err1\\n' >&2; printf 'out1\\n'; printf 'err2\\n' >&2; printf 'out2\\n'"
        }),
    );

    let err1 = output.content.find("err1").expect("stderr chunk");
    let out1 = output.content.find("out1").expect("stdout chunk");
    let err2 = output.content.find("err2").expect("second stderr chunk");
    let out2 = output.content.find("out2").expect("second stdout chunk");
    assert!(
        err1 < out1 && out1 < err2 && err2 < out2,
        "output should preserve chunk order: {}",
        output.content
    );
}

#[test]
fn bash_uses_bash_when_available() {
    if !std::path::Path::new("/bin/bash").exists() {
        return;
    }

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _guard = EnvVarGuard::unset("NAV_BASH_SHELL");

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
fn bash_shell_override_resolves_path_binary() {
    if std::process::Command::new("bash")
        .arg("-c")
        .arg("exit 0")
        .status()
        .is_err()
    {
        return;
    }

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _guard = EnvVarGuard::set("NAV_BASH_SHELL", "bash");

    let workspace = Workspace::new();
    let output = run(
        &workspace,
        "bash",
        json!({ "command": "echo ${BASH_VERSION}" }),
    );

    assert!(
        !output.content.trim().is_empty(),
        "PATH shell override should resolve bash: {}",
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

    assert_eq!(output.content, "redblue");
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
