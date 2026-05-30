use serde_json::{Value, json};
use std::path::Path;

use crate::tools::truncation::{TruncationOptions, TruncationStrategy, truncate_output};
use crate::workspace::shell::{ShellCommand, ShellTermination, run_shell_command_until};

use super::{
    NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolError, ToolFuture, ToolOutput,
    ToolRegistry, ToolRegistryError,
};

// ---------------------------------------------------------------------------
// Binary resolution
// ---------------------------------------------------------------------------

/// Resolve the `rg` binary path via `$PATH` lookup.
fn which_rg() -> Option<String> {
    let output = std::process::Command::new("which")
        .arg("rg")
        .output()
        .ok()?;
    if output.status.success() {
        Some(
            String::from_utf8(output.stdout)
                .ok()
                .map(|s| s.trim().to_string())?,
        )
    } else {
        None
    }
}

const RG_TIMEOUT_SECS: u64 = 30;

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

pub fn register(registry: &mut ToolRegistry) -> Result<(), ToolRegistryError> {
    registry.register(RipgrepTool::discover())?;
    registry.add_to_preset(super::ToolPreset::Coding, "ripgrep")?;
    registry.add_to_preset(super::ToolPreset::Readonly, "ripgrep")
}

pub struct RipgrepTool {
    rg_binary: Option<String>,
}

impl RipgrepTool {
    /// Resolve `rg` at construction time so the async execution path never
    /// blocks on a synchronous `which` lookup.
    pub(crate) fn discover() -> Self {
        Self {
            rg_binary: which_rg(),
        }
    }

    #[cfg(test)]
    fn with_binary(rg_binary: Option<String>) -> Self {
        Self { rg_binary }
    }
}

impl NavTool for RipgrepTool {
    fn name(&self) -> &str {
        "ripgrep"
    }

    fn description(&self) -> &str {
        "Search the codebase with ripgrep. Returns matching lines with file paths and line numbers."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("Search file contents with ripgrep")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Search pattern (regex by default, literal when literal is true)."
                },
                "path": {
                    "type": "string",
                    "description": "Workspace-relative or in-workspace absolute path to search. Defaults to the session cwd."
                },
                "glob": {
                    "type": "string",
                    "description": "Optional glob filter (e.g. \"*.rs\", \"*.ts\")."
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "Case-insensitive search when true."
                },
                "literal": {
                    "type": "boolean",
                    "description": "Treat pattern as a literal string when true."
                },
                "context": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Number of context lines before and after each match."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of result lines. Defaults to 100."
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn risk_class(&self) -> RiskClass {
        RiskClass::Search
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
        cancel: ToolCancellationToken,
    ) -> ToolFuture<'a> {
        Box::pin(async move { self.execute_ripgrep(ctx, args, cancel).await })
    }
}

impl RipgrepTool {
    async fn execute_ripgrep(
        &self,
        ctx: &ToolContext,
        args: Value,
        cancel: ToolCancellationToken,
    ) -> super::ToolResult {
        if cancel.is_cancelled() {
            return Err(ToolError::new("tool call cancelled"));
        }

        let rg_args = RipgrepArgs::parse(args)?;
        let policy = ctx
            .path_policy()
            .ok_or_else(|| ToolError::new("workspace path policy is not configured"))?;

        let search_path = if rg_args.path.is_empty() {
            policy.session_cwd().to_path_buf()
        } else {
            let resolved = policy
                .resolve(&rg_args.path)
                .map_err(|error| ToolError::new(error.to_string()))?;
            resolved.into_path()
        };

        let rg_binary = self.rg_binary.as_ref().ok_or_else(|| {
            ToolError::new("ripgrep (`rg`) is not installed. Install it with: brew install ripgrep")
        })?;

        let command = build_rg_command(rg_binary, &rg_args, &search_path);
        let cwd = policy.session_cwd().to_path_buf();

        let output = run_shell_command_until(
            ShellCommand {
                command,
                cwd,
                timeout: Some(std::time::Duration::from_secs(RG_TIMEOUT_SECS)),
            },
            cancel.cancelled(),
        )
        .await
        .map_err(|error| ToolError::new(error.to_string()))?;

        match output.termination {
            ShellTermination::Exited => {}
            ShellTermination::TimedOut => {
                return Err(ToolError::new(format!(
                    "ripgrep command timed out after {RG_TIMEOUT_SECS}s"
                )));
            }
            ShellTermination::Cancelled => return Err(ToolError::new("tool call cancelled")),
        }

        // rg exits with 1 when no matches found — that's not an error for us
        if output.status_code == Some(1) {
            return Ok(ToolOutput::text(String::new()));
        }

        if output.status_code != Some(0) {
            return Err(ToolError::new(format!(
                "ripgrep failed: {}",
                output.stderr.trim()
            )));
        }

        let limited = apply_limit(&output.stdout, rg_args.limit);
        let truncated = truncate_output(
            &limited,
            TruncationOptions {
                strategy: TruncationStrategy::Head,
                ..TruncationOptions::default()
            },
        );
        Ok(ToolOutput::text(truncated.render()))
    }
}

fn build_rg_command(binary: &str, args: &RipgrepArgs, search_path: &Path) -> String {
    let mut parts = vec![shell_quote_arg(binary)];

    parts.push("--with-filename".to_string());
    parts.push("--line-number".to_string());
    parts.push("--color".to_string());
    parts.push("never".to_string());

    if args.literal {
        parts.push("-F".to_string());
    }
    if args.ignore_case {
        parts.push("-i".to_string());
    }
    if let Some(context) = args.context {
        parts.push("-C".to_string());
        parts.push(context.to_string());
    }
    if let Some(glob) = &args.glob {
        parts.push("--glob".to_string());
        parts.push(shell_quote_arg(glob));
    }

    parts.push(shell_quote_arg(&args.pattern));
    parts.push(shell_quote_arg(&search_path.to_string_lossy()));

    parts.join(" ")
}

fn apply_limit(content: &str, limit: usize) -> String {
    content.lines().take(limit).collect::<Vec<_>>().join("\n")
}

fn shell_quote_arg(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RipgrepArgs {
    pattern: String,
    path: String,
    glob: Option<String>,
    ignore_case: bool,
    literal: bool,
    context: Option<usize>,
    limit: usize,
}

impl RipgrepArgs {
    fn parse(args: Value) -> Result<Self, ToolError> {
        let object = args
            .as_object()
            .ok_or_else(|| ToolError::new("ripgrep arguments must be an object"))?;
        reject_unknown_arguments(object)?;

        let pattern = object
            .get("pattern")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| ToolError::new("ripgrep argument `pattern` is required"))?
            .to_string();

        let path = object
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let glob = object.get("glob").and_then(Value::as_str).map(String::from);

        let ignore_case = object
            .get("ignore_case")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let literal = object
            .get("literal")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let context = super::parse_optional_positive_usize(object.get("context"), "context")?;

        let limit =
            super::parse_optional_positive_usize(object.get("limit"), "limit")?.unwrap_or(100);

        Ok(Self {
            pattern,
            path,
            glob,
            ignore_case,
            literal,
            context,
            limit,
        })
    }
}

const KNOWN_ARGUMENTS: &[&str] = &[
    "pattern",
    "path",
    "glob",
    "ignore_case",
    "literal",
    "context",
    "limit",
];

fn reject_unknown_arguments(object: &serde_json::Map<String, Value>) -> Result<(), ToolError> {
    for name in object.keys() {
        if !KNOWN_ARGUMENTS.contains(&name.as_str()) {
            return Err(ToolError::new(format!("unknown ripgrep argument `{name}`")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::RipgrepTool;
    use crate::tools::{
        NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolPreset, ToolRegistry,
    };
    use crate::workspace::path::WorkspacePathPolicy;
    use serde_json::json;

    fn rg_tool() -> RipgrepTool {
        RipgrepTool::discover()
    }

    // --- Registration ---

    #[test]
    fn registers_against_coding_and_readonly_presets() {
        let mut registry = ToolRegistry::new();
        super::register(&mut registry).expect("ripgrep should register");

        assert!(
            registry
                .preset_tool_names(ToolPreset::Coding)
                .contains(&"ripgrep".to_string()),
            "ripgrep should be in coding preset"
        );
        assert!(
            registry
                .preset_tool_names(ToolPreset::Readonly)
                .contains(&"ripgrep".to_string()),
            "ripgrep should be in readonly preset"
        );
        assert_eq!(
            registry
                .get("ripgrep")
                .expect("ripgrep should be registered")
                .risk_class(),
            RiskClass::Search,
        );
    }

    // --- Unknown arguments ---

    #[tokio::test]
    async fn rejects_unknown_arguments_before_execution() {
        let workspace = TestWorkspace::new("unknown_arg");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "test", "bogus": true }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("unknown argument should fail before execution");

        assert_eq!(error.message(), "unknown ripgrep argument `bogus`");
    }

    // --- Missing / empty pattern ---

    #[tokio::test]
    async fn rejects_missing_pattern_argument() {
        let workspace = TestWorkspace::new("missing_pattern");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = rg_tool()
            .execute(&context, json!({}), ToolCancellationToken::new())
            .await
            .expect_err("missing pattern should fail");

        assert_eq!(error.message(), "ripgrep argument `pattern` is required");
    }

    #[tokio::test]
    async fn rejects_whitespace_only_pattern() {
        let workspace = TestWorkspace::new("ws_pattern");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "   " }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("whitespace pattern should fail");

        assert_eq!(error.message(), "ripgrep argument `pattern` is required");
    }

    // --- Cancellation ---

    #[tokio::test]
    async fn returns_error_when_already_cancelled() {
        let workspace = TestWorkspace::new("cancelled");
        let context = ToolContext::with_path_policy(workspace.policy());
        let cancel = ToolCancellationToken::new();
        cancel.cancel();

        let error = rg_tool()
            .execute(&context, json!({ "pattern": "test" }), cancel)
            .await
            .expect_err("cancelled token should fail immediately");

        assert_eq!(error.message(), "tool call cancelled");
    }

    // --- Literal match ---

    #[tokio::test]
    async fn literal_match_returns_results() {
        let workspace = TestWorkspace::new("literal_match");
        workspace.write(
            "src/main.rs",
            "fn hello() {\n    println!(\"hello world\");\n}\n",
        );
        workspace.write("src/lib.rs", "fn goodbye() {\n    println!(\"bye\");\n}\n");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "hello", "literal": true }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("ripgrep should succeed");

        assert!(
            output.content.contains("hello"),
            "output should contain match: got {:?}",
            output.content
        );
        assert!(
            !output.content.contains("bye"),
            "output should not contain non-match"
        );
    }

    // --- Regex match ---

    #[tokio::test]
    async fn regex_match_returns_results() {
        let workspace = TestWorkspace::new("regex_match");
        workspace.write(
            "app.log",
            "error: disk full\ninfo: started\nerror: timeout\n",
        );
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "error:.*" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("ripgrep should succeed");

        assert!(
            output.content.contains("disk full"),
            "should match first error: {:?}",
            output.content
        );
        assert!(
            output.content.contains("timeout"),
            "should match second error: {:?}",
            output.content
        );
        assert!(
            !output.content.contains("started"),
            "should not match info line"
        );
    }

    // --- Glob filter ---

    #[tokio::test]
    async fn glob_filter_restricts_results_to_matching_files() {
        let workspace = TestWorkspace::new("glob_filter");
        workspace.write("src/main.rs", "let x = 42;");
        workspace.write("notes.txt", "let x = 42;");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "let x", "glob": "*.rs" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("ripgrep should succeed");

        assert!(
            output.content.contains("main.rs"),
            "should find rust file: {:?}",
            output.content
        );
        assert!(
            !output.content.contains("notes.txt"),
            "should not find txt file"
        );
    }

    // --- Context lines ---

    #[tokio::test]
    async fn context_lines_included_in_output() {
        let workspace = TestWorkspace::new("context_lines");
        workspace.write("data.txt", "line1\nline2\nTARGET\nline4\nline5");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "TARGET", "context": 1 }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("ripgrep should succeed");

        assert!(
            output.content.contains("line2"),
            "should include 1 line before: {:?}",
            output.content
        );
        assert!(output.content.contains("TARGET"), "should include match");
        assert!(
            output.content.contains("line4"),
            "should include 1 line after: {:?}",
            output.content
        );
        assert!(
            !output.content.contains("line1"),
            "should not include 2 lines before"
        );
        assert!(
            !output.content.contains("line5"),
            "should not include 2 lines after"
        );
    }

    // --- Limit truncation ---

    #[tokio::test]
    async fn limit_truncates_output_lines() {
        let workspace = TestWorkspace::new("limit_truncation");
        let lines: Vec<String> = (0..20).map(|i| format!("match_line_{i}")).collect();
        workspace.write("big.txt", &lines.join("\n"));
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "match_line", "limit": 5 }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("ripgrep should succeed");

        let result_lines: Vec<&str> = output.content.lines().collect();
        assert_eq!(
            result_lines.len(),
            5,
            "limit should cap to 5 lines: got {:?}",
            output.content
        );
    }

    // --- No matches returns empty result ---

    #[tokio::test]
    async fn no_matches_returns_empty_result_not_error() {
        let workspace = TestWorkspace::new("no_matches");
        workspace.write("empty.txt", "nothing relevant here");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "NONEXISTENT_PATTERN_XYZ" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("ripgrep should succeed for no matches");

        assert_eq!(output.content, "", "no matches should return empty content");
    }

    // --- Missing binary ---

    #[tokio::test]
    async fn missing_binary_returns_structured_error_with_install_hint() {
        let workspace = TestWorkspace::new("missing_binary");
        workspace.write("dummy.txt", "content");
        let context = ToolContext::with_path_policy(workspace.policy());

        let tool = RipgrepTool::with_binary(None);

        let error = tool
            .execute(
                &context,
                json!({ "pattern": "content" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("missing rg should return an error");

        assert!(
            error.message().contains("rg"),
            "error should mention rg: {:?}",
            error.message()
        );
        assert!(
            error.message().contains("brew install ripgrep"),
            "error should include install hint: {:?}",
            error.message()
        );
    }

    // --- Ignore case ---

    #[tokio::test]
    async fn ignore_case_finds_case_insensitive_matches() {
        let workspace = TestWorkspace::new("ignore_case");
        workspace.write("data.txt", "Hello World");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "hello world", "ignore_case": true }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("ripgrep should succeed");

        assert!(
            output.content.contains("Hello World"),
            "should find case-insensitive match: {:?}",
            output.content
        );
    }

    // --- Path outside workspace rejected ---

    #[tokio::test]
    async fn path_outside_workspace_rejected_by_policy() {
        let workspace = TestWorkspace::new("path_escape");
        workspace.write("safe.txt", "content");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = rg_tool()
            .execute(
                &context,
                json!({ "pattern": "anything", "path": "/etc/passwd" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("path outside workspace should be rejected");

        assert!(
            error.message().contains("outside allowed roots"),
            "should mention policy violation: {:?}",
            error.message()
        );
    }

    // --- Test workspace helper ---

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("nav-rg-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(&root).expect("workspace should canonicalize"),
            }
        }

        fn write(&self, relative_path: &str, content: &str) {
            if let Some(parent) = self.root.join(relative_path).parent() {
                fs::create_dir_all(parent).expect("parent dir should be created");
            }
            fs::write(self.root.join(relative_path), content).expect("file should be written");
        }

        fn policy(&self) -> WorkspacePathPolicy {
            WorkspacePathPolicy::new(&self.root, &self.root)
                .expect("path policy should accept workspace")
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
