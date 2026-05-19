//! Unsandboxed shell runner — same behavior as the pre-permissions
//! `tools/shell.rs::bash`. Used on platforms without an OS sandbox impl yet,
//! and intentionally selected by `DangerFullAccess`.

use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::{process::Command, time};

use super::{SandboxOutput, SandboxRequest, SandboxRunner};

pub struct PassthroughRunner;

impl SandboxRunner for PassthroughRunner {
    fn run<'a>(
        &'a self,
        req: SandboxRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SandboxOutput>> + Send + 'a>>
    {
        Box::pin(async move { run_with_command(req, "sh", &["-c"]).await })
    }
}

/// Spawn `command` with the given prefix args followed by the user's command
/// string. Used by both `PassthroughRunner` and the Seatbelt runner so the
/// timeout, capture, and status-string handling stay identical.
pub(super) async fn run_with_command(
    req: SandboxRequest,
    program: &str,
    prefix_args: &[&str],
) -> Result<SandboxOutput> {
    let mut cmd = Command::new(program);
    cmd.kill_on_drop(true)
        .args(prefix_args)
        .arg(&req.command)
        .current_dir(&req.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn command `{}`", req.command))?;

    // Race the child against the timeout AND the abort signal. The first
    // arm that resolves wins. `kill_on_drop` above means dropping the
    // partially-consumed child sends SIGKILL, so the abort arm shedding
    // its `child` borrow is enough to clean up.
    let wait_fut = child.wait_with_output();
    let output = tokio::select! {
        biased;
        _ = req.abort.wait() => {
            bail!("command aborted by user: {}", req.command);
        }
        result = time::timeout(req.timeout, wait_fut) => match result {
            Ok(output) => output?,
            Err(_) => bail!(
                "command timed out after {}s: {}",
                req.timeout.as_secs(),
                req.command
            ),
        },
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let status_display = format!("{}", output.status);
    Ok(SandboxOutput {
        stdout,
        stderr,
        status: output.status.code(),
        status_display,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::SandboxPolicy;
    use std::path::PathBuf;
    use std::time::Duration;

    fn req(command: &str, timeout_secs: u64) -> SandboxRequest {
        SandboxRequest {
            command: command.into(),
            cwd: PathBuf::from("/tmp"),
            timeout: Duration::from_secs(timeout_secs),
            policy: SandboxPolicy::DangerFullAccess,
            abort: crate::agent::AbortSignal::default(),
        }
    }

    #[tokio::test]
    async fn passthrough_echo() {
        let out = PassthroughRunner.run(req("echo hi", 5)).await.unwrap();
        assert!(out.stdout.contains("hi"));
        assert_eq!(out.status, Some(0));
    }

    #[tokio::test]
    async fn passthrough_captures_stderr_separately() {
        let out = PassthroughRunner.run(req("echo err >&2", 5)).await.unwrap();
        assert!(out.stderr.contains("err"));
        assert!(!out.stdout.contains("err"));
    }

    #[tokio::test]
    async fn passthrough_reports_exit_code() {
        let out = PassthroughRunner.run(req("exit 42", 5)).await.unwrap();
        assert_eq!(out.status, Some(42));
        assert!(out.status_display.contains("42"));
    }

    #[tokio::test]
    async fn passthrough_times_out() {
        let err = PassthroughRunner.run(req("sleep 60", 1)).await.unwrap_err();
        assert!(err.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn passthrough_aborts_long_running_command_quickly() {
        use std::time::Instant;
        // 60s sleep — without the abort race the runner would either return
        // with a timeout error or sit until the child exited. Tripping the
        // abort half a second in must return promptly with a "command
        // aborted" error rather than running the full timeout window.
        let abort = crate::agent::AbortSignal::new();
        let req = SandboxRequest {
            command: "sleep 60".into(),
            cwd: PathBuf::from("/tmp"),
            timeout: Duration::from_secs(30),
            policy: SandboxPolicy::DangerFullAccess,
            abort: abort.clone(),
        };
        let started = Instant::now();
        let handle = tokio::spawn(async move { PassthroughRunner.run(req).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        abort.trip("test");
        let result = handle.await.unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "abort must return promptly, took {elapsed:?}"
        );
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("aborted by user"),
            "expected abort error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn passthrough_runs_in_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let req = SandboxRequest {
            command: "pwd".into(),
            cwd: cwd.clone(),
            timeout: Duration::from_secs(5),
            policy: SandboxPolicy::DangerFullAccess,
            abort: crate::agent::AbortSignal::default(),
        };
        let out = PassthroughRunner.run(req).await.unwrap();
        assert!(out.stdout.contains(&cwd.display().to_string()));
    }
}
