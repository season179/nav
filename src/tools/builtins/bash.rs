//! `bash` — run a shell command in the workspace, time-bounded and cancelable.
//!
//! In an ordinary checkout, `bash` keeps the trusted-local posture: it runs with
//! the backend user's shell privileges. In a linked git worktree, it adds the
//! same practical guardrails as the path tools: main-checkout paths are
//! redirected to the active worktree, sibling worktree paths and `..` traversal
//! are blocked. It is bounded by a timeout and the cancel flag, and its output
//! is capped.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

use super::support::truncate::{cap_tail, cap_tail_with_meta};
use super::support::worktree::rewrite_bash_command;
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_opt_u64, arg_str};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const SHELL_OVERRIDE_ENV: &str = "NAV_BASH_SHELL";

pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command in the working directory. Returns combined \
         stdout and stderr. Output is truncated to the last 2000 lines or 50KB; \
         if truncated, the full output is saved to a temp file. Provide an \
         optional timeout in seconds (default 120)."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Execute bash commands (ls, grep, find, etc.)")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Bash command to execute" },
                "timeout": { "type": "integer", "description": "Timeout in seconds (default 120)" }
            },
            "required": ["command"]
        })
    }

    fn execute(
        &self,
        args: &Value,
        cwd: &Path,
        cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError> {
        let command =
            rewrite_bash_command(cwd, arg_str(args, "command")?).map_err(ToolError::new)?;
        let timeout =
            Duration::from_secs(arg_opt_u64(args, "timeout").unwrap_or(DEFAULT_TIMEOUT_SECS));

        let runner = LocalBashRunner::from_env()?;
        let run = runner.run(&command, cwd, timeout, cancel)?;

        Ok(ToolOutput::new(format_tool_output(run)))
    }
}

struct LocalBashRunner {
    shell: ShellConfig,
}

impl LocalBashRunner {
    fn from_env() -> Result<Self, ToolError> {
        Ok(Self {
            shell: ShellConfig::resolve()?,
        })
    }
    fn run(
        &self,
        command: &str,
        cwd: &Path,
        timeout: Duration,
        cancel: &CancelFlag,
    ) -> Result<BashRun, ToolError> {
        let output = OutputCapture::new().map_err(|error| {
            ToolError::new(format!("could not create command output capture: {error}"))
        })?;
        let stdout = output.stdio().map_err(|error| {
            ToolError::new(format!("could not capture command stdout: {error}"))
        })?;
        let stderr = output.stdio().map_err(|error| {
            ToolError::new(format!("could not capture command stderr: {error}"))
        })?;

        let mut child = self.shell.command(command);
        child
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        configure_child_process(&mut child);

        let child = child
            .spawn()
            .map_err(|error| ToolError::new(format!("could not start command: {error}")))?;

        collect_child_output(child, output, timeout, cancel)
    }
}

#[derive(Debug, Clone)]
struct ShellConfig {
    program: PathBuf,
    args: Vec<String>,
}

impl ShellConfig {
    fn bash(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: vec!["-c".to_owned()],
        }
    }

    fn resolve() -> Result<Self, ToolError> {
        if let Ok(custom) = env::var(SHELL_OVERRIDE_ENV) {
            let custom = custom.trim();
            if !custom.is_empty() {
                let path = PathBuf::from(custom);
                if path.exists() {
                    return Ok(Self::bash(path));
                }
                if path.components().count() == 1
                    && let Some(found) = find_on_path(custom).into_iter().next()
                {
                    return Ok(Self::bash(found));
                }
                return Err(ToolError::new(format!(
                    "{SHELL_OVERRIDE_ENV} points to a missing shell: {custom}"
                )));
            }
        }

        default_shell_config()
    }

    fn command(&self, command: &str) -> Command {
        let mut process = Command::new(&self.program);
        process.args(&self.args).arg(command);
        process
    }
}

#[cfg(windows)]
fn default_shell_config() -> Result<ShellConfig, ToolError> {
    let mut candidates = Vec::new();
    if let Some(program_files) = env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(program_files).join("Git\\bin\\bash.exe"));
    }
    if let Some(program_files_x86) = env::var_os("ProgramFiles(x86)") {
        candidates.push(PathBuf::from(program_files_x86).join("Git\\bin\\bash.exe"));
    }
    candidates.extend(find_on_path("bash.exe"));

    for candidate in candidates {
        if candidate.exists() {
            return Ok(ShellConfig::bash(candidate));
        }
    }

    Err(ToolError::new(
        "no bash shell found; install Git Bash or set NAV_BASH_SHELL",
    ))
}

#[cfg(not(windows))]
fn default_shell_config() -> Result<ShellConfig, ToolError> {
    let bin_bash = PathBuf::from("/bin/bash");
    if bin_bash.exists() {
        return Ok(ShellConfig::bash(bin_bash));
    }

    if let Some(bash) = find_on_path("bash").into_iter().next() {
        return Ok(ShellConfig::bash(bash));
    }

    Ok(ShellConfig::bash("sh"))
}

fn find_on_path(binary: &str) -> Vec<PathBuf> {
    let Some(path) = env::var_os("PATH") else {
        return Vec::new();
    };
    env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .filter(|candidate| candidate.exists())
        .collect()
}

#[cfg(unix)]
fn configure_child_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_child_process(_command: &mut Command) {}

#[derive(Debug)]
struct BashRun {
    output: String,
    outcome: Outcome,
}

#[derive(Debug)]
enum Outcome {
    Exited(Option<i32>),
    TimedOut(u64),
    Cancelled,
    Error(String),
}

fn format_tool_output(run: BashRun) -> String {
    let mut combined = run.output;

    let note = match run.outcome {
        Outcome::Exited(Some(0)) => None,
        Outcome::Exited(Some(code)) => Some(format!("[exited with status {code}]")),
        Outcome::Exited(None) => Some("[terminated by signal]".to_owned()),
        Outcome::TimedOut(secs) => Some(format!("[timed out after {secs}s]")),
        Outcome::Cancelled => Some("[cancelled]".to_owned()),
        Outcome::Error(error) => Some(format!("[error waiting for command: {error}]")),
    };
    if let Some(note) = note {
        append_with_newline(&mut combined, &note);
    }

    let capped = cap_tail_with_meta(&combined);
    if !capped.truncated {
        return capped.content;
    }

    let footer = match write_full_output(&combined) {
        Ok(path) => format!("[Full output: {}]", path.display()),
        Err(error) => format!("[Full output could not be saved: {error}]"),
    };
    let mut with_footer = capped.content;
    append_with_blank_line(&mut with_footer, &footer);
    cap_tail(&with_footer)
}

fn append_with_newline(text: &mut String, suffix: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(suffix);
}

fn append_with_blank_line(text: &mut String, suffix: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(suffix);
}

fn write_full_output(output: &str) -> io::Result<PathBuf> {
    let path = env::temp_dir().join(format!("nav-bash-{}.log", Uuid::now_v7()));
    fs::write(&path, output)?;
    Ok(path)
}

struct OutputCapture {
    path: PathBuf,
}

impl OutputCapture {
    fn new() -> io::Result<Self> {
        let path = env::temp_dir().join(format!("nav-bash-capture-{}.log", Uuid::now_v7()));
        File::create_new(&path)?;
        Ok(Self { path })
    }

    fn stdio(&self) -> io::Result<Stdio> {
        let file = OpenOptions::new().append(true).open(&self.path)?;
        Ok(Stdio::from(file))
    }

    fn read_sanitized(&self) -> io::Result<String> {
        let bytes = fs::read(&self.path)?;
        Ok(sanitize_output_chunk(&bytes))
    }
}

impl Drop for OutputCapture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn collect_child_output(
    mut child: Child,
    output: OutputCapture,
    timeout: Duration,
    cancel: &CancelFlag,
) -> Result<BashRun, ToolError> {
    let deadline = Instant::now() + timeout;
    let outcome = loop {
        if let Some(done) = poll_child_outcome(&mut child) {
            break done;
        }
        if let Some(stop) = requested_stop(cancel, deadline, timeout) {
            kill_process_tree(&mut child);
            let _ = child.wait();
            break stop;
        }
        thread::sleep(POLL_INTERVAL);
    };

    let output = output
        .read_sanitized()
        .map_err(|error| ToolError::new(format!("could not read command output: {error}")))?;

    Ok(BashRun { output, outcome })
}

fn poll_child_outcome(child: &mut Child) -> Option<Outcome> {
    match child.try_wait() {
        Ok(Some(status)) => Some(Outcome::Exited(status.code())),
        Ok(None) => None,
        Err(error) => Some(Outcome::Error(error.to_string())),
    }
}

fn requested_stop(cancel: &CancelFlag, deadline: Instant, timeout: Duration) -> Option<Outcome> {
    if cancel.load(Ordering::Relaxed) {
        Some(Outcome::Cancelled)
    } else if Instant::now() >= deadline {
        Some(Outcome::TimedOut(timeout.as_secs()))
    } else {
        None
    }
}

fn sanitize_output_chunk(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    strip_ansi(&text)
        .chars()
        .filter(|&ch| is_display_safe(ch))
        .collect()
}

fn is_display_safe(ch: char) -> bool {
    let code = ch as u32;
    ch == '\n' || ch == '\t' || (code >= 0x20 && !(0xfff9..=0xfffb).contains(&code) && code != 0x7f)
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for seq in chars.by_ref() {
                    if ('@'..='~').contains(&seq) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                let mut previous = '\0';
                for seq in chars.by_ref() {
                    if seq == '\x07' || (previous == '\x1b' && seq == '\\') {
                        break;
                    }
                    previous = seq;
                }
            }
            _ => {}
        }
    }
    out
}

fn kill_process_tree(child: &mut Child) {
    kill_process_tree_by_pid(child.id());
    let _ = child.kill();
}

#[cfg(unix)]
fn kill_process_tree_by_pid(pid: u32) {
    let pid = pid as libc::pid_t;
    // SAFETY: negative pid targets the process group created for the child.
    let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
    if result < 0 {
        // SAFETY: fallback to the child pid if the process group is already gone.
        let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

#[cfg(windows)]
fn kill_process_tree_by_pid(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(not(any(unix, windows)))]
fn kill_process_tree_by_pid(_pid: u32) {}
