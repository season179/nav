//! `bash` — run a shell command in the workspace, time-bounded and cancelable.
//!
//! Unlike the path tools, `bash` is not confined to the workspace: it runs with
//! the backend user's shell privileges (the trusted-local posture). It is
//! bounded by a timeout and the cancel flag, and its output is capped.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::support::truncate::cap_tail;
use super::{CancelFlag, Tool, ToolError, ToolOutput, arg_opt_u64, arg_str};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command in the working directory. Returns combined \
         stdout and stderr. Output is truncated to the last 2000 lines or 50KB. \
         Provide an optional timeout in seconds (default 120)."
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

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| ToolError::new(format!("could not start command: {error}")))?;

        // Drain both pipes on threads so a chatty command can't deadlock by
        // filling a pipe buffer while we wait.
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let stdout_reader = thread::spawn(move || drain(stdout.as_mut()));
        let stderr_reader = thread::spawn(move || drain(stderr.as_mut()));

        let deadline = Instant::now() + timeout;
        let outcome = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Outcome::Exited(status.code()),
                Ok(None) => {}
                Err(error) => break Outcome::Error(error.to_string()),
            }
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                break Outcome::Cancelled;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                break Outcome::TimedOut(timeout.as_secs());
            }
            thread::sleep(POLL_INTERVAL);
        };

        // The reader threads finish once the pipes close (on exit or kill).
        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();

        let mut combined = String::new();
        combined.push_str(&stdout);
        if !stderr.is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&stderr);
        }

        let note = match outcome {
            Outcome::Exited(Some(0)) => None,
            Outcome::Exited(Some(code)) => Some(format!("[exited with status {code}]")),
            Outcome::Exited(None) => Some("[terminated by signal]".to_owned()),
            Outcome::TimedOut(secs) => Some(format!("[timed out after {secs}s]")),
            Outcome::Cancelled => Some("[cancelled]".to_owned()),
            Outcome::Error(error) => Some(format!("[error waiting for command: {error}]")),
        };
        if let Some(note) = note {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&note);
        }

        Ok(ToolOutput::new(cap_tail(&combined)))
    }
}

enum Outcome {
    Exited(Option<i32>),
    TimedOut(u64),
    Cancelled,
    Error(String),
}

fn drain(pipe: Option<&mut impl Read>) -> String {
    let mut buffer = String::new();
    if let Some(pipe) = pipe {
        let _ = pipe.read_to_string(&mut buffer);
    }
    buffer
}
