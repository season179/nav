//! `bash` — run a shell command in the workspace, time-bounded and cancelable.
//!
//! Unlike the path tools, `bash` is not confined to the workspace: it runs with
//! the backend user's shell privileges (the trusted-local posture). It is
//! bounded by a timeout and the cancel flag, and its output is capped.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

use super::support::truncate::{cap_tail, cap_tail_with_meta};
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_opt_u64, arg_str};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const EXIT_PIPE_GRACE: Duration = Duration::from_millis(100);
const READ_BUFFER_BYTES: usize = 8192;
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
        let command = arg_str(args, "command")?;
        let timeout =
            Duration::from_secs(arg_opt_u64(args, "timeout").unwrap_or(DEFAULT_TIMEOUT_SECS));

        let runner = LocalBashRunner::from_env()?;
        let run = runner.run(command, cwd, timeout, cancel)?;

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
        let mut child = self.shell.command(command);
        child
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_child_process(&mut child);

        let mut child = child
            .spawn()
            .map_err(|error| ToolError::new(format!("could not start command: {error}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::new("could not capture command stdout pipe after spawn"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ToolError::new("could not capture command stderr pipe after spawn"))?;

        collect_child_output(child, stdout, stderr, timeout, cancel)
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

#[cfg(unix)]
fn collect_child_output(
    mut child: Child,
    mut stdout: ChildStdout,
    mut stderr: ChildStderr,
    timeout: Duration,
    cancel: &CancelFlag,
) -> Result<BashRun, ToolError> {
    set_nonblocking(&stdout)
        .map_err(|error| ToolError::new(format!("could not set stdout nonblocking: {error}")))?;
    set_nonblocking(&stderr)
        .map_err(|error| ToolError::new(format!("could not set stderr nonblocking: {error}")))?;

    let deadline = Instant::now() + timeout;
    let mut output = String::new();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut outcome: Option<Outcome> = None;
    let mut exited_at: Option<Instant> = None;

    loop {
        drain_if_open(&mut stdout_open, &mut stdout, &mut output)?;
        drain_if_open(&mut stderr_open, &mut stderr, &mut output)?;

        if outcome.is_none()
            && let Some(done) = poll_child_outcome(&mut child)
        {
            outcome = Some(done);
            exited_at = Some(Instant::now());
        }

        if outcome.is_none()
            && let Some(stop) = requested_stop(cancel, deadline, timeout)
        {
            kill_process_tree(&mut child);
            let _ = child.wait();
            outcome = Some(stop);
            exited_at = Some(Instant::now());
        }

        if outcome.is_some() && command_output_is_done(stdout_open, stderr_open, exited_at) {
            break;
        }

        thread::sleep(POLL_INTERVAL);
    }

    Ok(BashRun {
        output,
        outcome: outcome.unwrap_or_else(|| Outcome::Error("command ended unexpectedly".to_owned())),
    })
}

#[cfg(not(unix))]
fn collect_child_output(
    mut child: Child,
    stdout: ChildStdout,
    stderr: ChildStderr,
    timeout: Duration,
    cancel: &CancelFlag,
) -> Result<BashRun, ToolError> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<String>();
    let stdout_tx = tx.clone();
    let stderr_tx = tx;
    thread::spawn(move || drain_blocking(stdout, stdout_tx));
    thread::spawn(move || drain_blocking(stderr, stderr_tx));

    let deadline = Instant::now() + timeout;
    let mut output = String::new();
    let outcome = loop {
        while let Ok(chunk) = rx.try_recv() {
            output.push_str(&chunk);
        }

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

    let drain_until = Instant::now() + EXIT_PIPE_GRACE;
    while Instant::now() < drain_until {
        while let Ok(chunk) = rx.try_recv() {
            output.push_str(&chunk);
        }
        thread::sleep(POLL_INTERVAL);
    }

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

#[cfg(unix)]
fn drain_if_open(
    open: &mut bool,
    pipe: &mut impl Read,
    output: &mut String,
) -> Result<(), ToolError> {
    if *open {
        *open = drain_available(pipe, output)?;
    }
    Ok(())
}

#[cfg(unix)]
fn command_output_is_done(
    stdout_open: bool,
    stderr_open: bool,
    exited_at: Option<Instant>,
) -> bool {
    (!stdout_open && !stderr_open) || exited_at.is_some_and(|at| at.elapsed() >= EXIT_PIPE_GRACE)
}

#[cfg(not(unix))]
fn drain_blocking(mut pipe: impl Read, tx: std::sync::mpsc::Sender<String>) {
    let mut buffer = [0; READ_BUFFER_BYTES];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(sanitize_output_chunk(&buffer[..n])).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(unix)]
fn set_nonblocking<T: std::os::fd::AsRawFd>(pipe: &T) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: fcntl is called with a live pipe fd owned by ChildStdout/Stderr.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: same fd as above; O_NONBLOCK only changes this descriptor's mode.
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn drain_available(pipe: &mut impl Read, output: &mut String) -> Result<bool, ToolError> {
    let mut buffer = [0; READ_BUFFER_BYTES];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(false),
            Ok(n) => output.push_str(&sanitize_output_chunk(&buffer[..n])),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(ToolError::new(format!(
                    "could not read command output: {error}"
                )));
            }
        }
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
