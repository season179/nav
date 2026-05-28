use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::tools::truncation::{TruncationOptions, TruncationStrategy, truncate_output};
use crate::workspace::shell::{
    ShellCommand, ShellOutputChunk, ShellTermination, run_shell_command_streaming_until,
};

use super::{
    NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolError, ToolFuture, ToolOutput,
    ToolRegistry, ToolRegistryError,
};

pub fn register(registry: &mut ToolRegistry) -> Result<(), ToolRegistryError> {
    registry.register(BashTool)?;
    registry.add_to_preset(super::ToolPreset::Coding, "bash")
}

#[derive(Debug, Clone, Copy)]
pub struct BashTool;

impl NavTool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command from the session cwd."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to run from the session cwd."
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional timeout in seconds."
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn risk_class(&self) -> RiskClass {
        RiskClass::Exec
    }

    fn streams_output(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
        cancel: ToolCancellationToken,
    ) -> ToolFuture<'a> {
        Box::pin(async move { execute_bash(ctx, args, cancel).await })
    }
}

async fn execute_bash(
    ctx: &ToolContext,
    args: Value,
    cancel: ToolCancellationToken,
) -> super::ToolResult {
    if cancel.is_cancelled() {
        return Err(ToolError::new("tool call cancelled"));
    }

    let BashArgs { command, timeout } = BashArgs::parse(args)?;
    let cwd = ctx
        .path_policy()
        .ok_or_else(|| ToolError::new("workspace path policy is not configured"))?
        .session_cwd()
        .to_path_buf();
    let output_sink = ctx.output_sink().cloned();
    let output = run_shell_command_streaming_until(
        ShellCommand {
            command,
            cwd: cwd.clone(),
            timeout: timeout.map(|seconds| Duration::from_secs(seconds as u64)),
        },
        cancel.cancelled(),
        move |chunk| emit_shell_output_chunk(output_sink.as_ref(), chunk),
    )
    .await
    .map_err(|error| ToolError::new(error.to_string()))?;

    let content = render_command_output(&output.stdout, &output.stderr);
    let visible_content = render_bounded_command_output(&cwd, &content)?;

    match output.termination {
        ShellTermination::Exited => {}
        ShellTermination::TimedOut => {
            return Err(ToolError::with_output(
                format!(
                    "command timed out after {}s",
                    timeout.expect("timeout termination should include timeout")
                ),
                visible_content,
            ));
        }
        ShellTermination::Cancelled => return Err(ToolError::new("tool call cancelled")),
    }

    if output.status_code == Some(0) {
        return Ok(ToolOutput::text(visible_content));
    }

    Err(ToolError::with_output(
        format!(
            "command exited with status {}",
            render_status_code(output.status_code)
        ),
        visible_content,
    ))
}

fn emit_shell_output_chunk(output_sink: Option<&super::ToolOutputSink>, chunk: ShellOutputChunk) {
    if let Some(output_sink) = output_sink {
        output_sink.push_chunk(chunk.stream.name(), chunk.chunk);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BashArgs {
    command: String,
    timeout: Option<usize>,
}

impl BashArgs {
    fn parse(args: Value) -> Result<Self, ToolError> {
        let object = args
            .as_object()
            .ok_or_else(|| ToolError::new("bash arguments must be an object"))?;
        reject_unknown_arguments(object)?;
        let command = object
            .get("command")
            .and_then(Value::as_str)
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| ToolError::new("bash argument `command` is required"))?
            .to_string();
        let timeout = super::parse_optional_positive_usize(object.get("timeout"), "timeout")?;

        Ok(Self { command, timeout })
    }
}

fn reject_unknown_arguments(object: &serde_json::Map<String, Value>) -> Result<(), ToolError> {
    for name in object.keys() {
        if name != "command" && name != "timeout" {
            return Err(ToolError::new(format!("unknown bash argument `{name}`")));
        }
    }

    Ok(())
}

fn bash_truncation_options() -> TruncationOptions {
    TruncationOptions {
        strategy: TruncationStrategy::Tail,
        ..TruncationOptions::default()
    }
}

fn render_status_code(status_code: Option<i32>) -> String {
    status_code.map_or_else(|| "unknown".to_string(), |code| code.to_string())
}

fn render_command_output(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, true) => stdout.to_string(),
        (true, false) => format!("stderr:\n{stderr}"),
        (false, false) => format!("stdout:\n{stdout}\nstderr:\n{stderr}"),
        (true, true) => String::new(),
    }
}

fn render_bounded_command_output(cwd: &Path, content: &str) -> Result<String, ToolError> {
    let truncated = truncate_output(content, bash_truncation_options());
    if !truncated.truncated() {
        return Ok(truncated.render());
    }

    let spill_path = spill_full_output(cwd, content)?;
    Ok(format!(
        "{}\nFull output: {}",
        truncated.render(),
        spill_path.display()
    ))
}

fn spill_full_output(cwd: &Path, content: &str) -> Result<PathBuf, ToolError> {
    static SPILL_COUNTER: AtomicU64 = AtomicU64::new(0);

    let output_dir = cwd.join(".nav/tool-output");
    fs::create_dir_all(&output_dir).map_err(|error| {
        ToolError::new(format!(
            "failed to create tool-output directory `{}`: {error}",
            output_dir.display()
        ))
    })?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = SPILL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = output_dir.join(format!(
        "bash-output-{timestamp}-{}-{counter}.txt",
        std::process::id()
    ));
    fs::write(&path, content).map_err(|error| {
        ToolError::new(format!(
            "failed to write spilled bash output `{}`: {error}",
            path.display()
        ))
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::{BashTool, register};
    use crate::tools::{
        NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolPreset, ToolRegistry,
    };
    use crate::workspace::path::WorkspacePathPolicy;

    #[tokio::test]
    async fn bash_tool_runs_successful_command_from_session_cwd() {
        let workspace = TestWorkspace::new("success");
        workspace.write("input.txt", "hello");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = BashTool
            .execute(
                &context,
                json!({ "command": "cat input.txt" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("bash command should succeed");

        assert_eq!(output.content, "hello");
    }

    #[tokio::test]
    async fn bash_tool_rejects_unknown_arguments_before_execution() {
        let workspace = TestWorkspace::new("unknown_arg");
        let marker = workspace.root.join("ran.txt");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = BashTool
            .execute(
                &context,
                json!({
                    "command": format!("printf run > {}", shell_quote(&marker)),
                    "cwd": "/tmp",
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("unknown argument should fail before command execution");

        assert_eq!(error.message(), "unknown bash argument `cwd`");
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn bash_tool_reports_non_zero_exit_with_output() {
        let workspace = TestWorkspace::new("nonzero");
        let context = ToolContext::with_path_policy(workspace.policy());

        let error = BashTool
            .execute(
                &context,
                json!({ "command": "printf stdout; printf stderr >&2; exit 7" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("non-zero bash command should fail");

        assert_eq!(error.message(), "command exited with status 7");
        assert_eq!(error.output(), Some("stdout:\nstdout\nstderr:\nstderr"));
    }

    #[tokio::test]
    async fn bash_tool_times_out_long_running_commands() {
        let workspace = TestWorkspace::new("timeout");
        let context = ToolContext::with_path_policy(workspace.policy());
        let started = Instant::now();

        let error = BashTool
            .execute(
                &context,
                json!({ "command": "sleep 2; printf done", "timeout": 1 }),
                ToolCancellationToken::new(),
            )
            .await
            .expect_err("bash command should time out");

        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(error.message().contains("timed out after 1s"));
    }

    #[tokio::test]
    async fn bash_tool_truncates_and_spills_large_output() {
        let workspace = TestWorkspace::new("spill");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = BashTool
            .execute(
                &context,
                json!({
                    "command": "for i in {1..6000}; do printf 'line%04d abcdefghijklmnopqrstuvwxyz\\n' \"$i\"; done"
                }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("large bash output should succeed");

        assert!(
            output
                .content
                .contains(crate::tools::truncation::TRUNCATED_MARKER)
        );
        let spill_path = extract_spill_path(&output.content);
        assert!(spill_path.starts_with(workspace.root.join(".nav/tool-output")));
        let full_output = fs::read_to_string(spill_path).expect("spill file should be readable");
        assert!(full_output.contains("line0001 abcdefghijklmnopqrstuvwxyz"));
        assert!(full_output.contains("line6000 abcdefghijklmnopqrstuvwxyz"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_tool_cancels_the_child_process_group() {
        let workspace = TestWorkspace::new("cancel_group");
        let context = ToolContext::with_path_policy(workspace.policy());
        let child_pid_path = workspace.root.join("child.pid");
        let command = format!(
            "(sleep 30) & echo $! > {}; wait",
            shell_quote(&child_pid_path)
        );
        let cancel = ToolCancellationToken::new();
        let cancel_for_task = cancel.clone();

        let task = tokio::spawn(async move {
            BashTool
                .execute(&context, json!({ "command": command }), cancel_for_task)
                .await
        });

        let child_pid = read_pid_file(&child_pid_path).await;
        cancel.cancel();
        let error = task
            .await
            .expect("bash task should join")
            .expect_err("cancelled command should return an error");

        assert_eq!(error.message(), "tool call cancelled");
        assert_process_exits(child_pid).await;
    }

    #[tokio::test]
    async fn bash_tool_inherits_only_documented_parent_environment() {
        let workspace = TestWorkspace::new("env");
        let context = ToolContext::with_path_policy(workspace.policy());

        let output = BashTool
            .execute(
                &context,
                json!({ "command": "env" }),
                ToolCancellationToken::new(),
            )
            .await
            .expect("env command should succeed");

        for line in output.content.lines() {
            let Some((name, _value)) = line.split_once('=') else {
                continue;
            };
            assert!(
                matches!(
                    name,
                    "PATH" | "HOME" | "USER" | "LANG" | "TERM" | "PWD" | "SHLVL" | "_"
                ),
                "unexpected inherited env var in bash output: {line}"
            );
        }
    }

    #[test]
    fn bash_tool_registers_for_coding_preset_only() {
        let mut registry = ToolRegistry::default();

        register(&mut registry).expect("bash should register");

        assert_eq!(registry.preset_tool_names(ToolPreset::Coding), vec!["bash"]);
        assert!(registry.preset_tool_names(ToolPreset::Readonly).is_empty());
        assert_eq!(
            registry
                .get("bash")
                .expect("bash should be registered")
                .risk_class(),
            RiskClass::Exec
        );
    }

    fn extract_spill_path(content: &str) -> PathBuf {
        let prefix = "Full output: ";
        content
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .map(PathBuf::from)
            .expect("tool output should reference spilled output path")
    }

    fn shell_quote(path: &std::path::Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }

    async fn read_pid_file(path: &std::path::Path) -> u32 {
        for _ in 0..100 {
            if let Ok(content) = fs::read_to_string(path) {
                return content
                    .trim()
                    .parse()
                    .expect("pid file should contain a process id");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        panic!("pid file was not written: {}", path.display());
    }

    #[cfg(unix)]
    async fn assert_process_exits(pid: u32) {
        for _ in 0..100 {
            if !process_exists(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        panic!("process {pid} should have exited after cancellation");
    }

    #[cfg(unix)]
    fn process_exists(pid: u32) -> bool {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            return true;
        }

        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("nav-bash-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(root).expect("workspace should canonicalize"),
            }
        }

        fn write(&self, relative_path: &str, content: &str) {
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
