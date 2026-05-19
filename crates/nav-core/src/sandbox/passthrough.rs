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

    let output = match time::timeout(req.timeout, child.wait_with_output()).await {
        Ok(output) => output?,
        Err(_) => bail!(
            "command timed out after {}s: {}",
            req.timeout.as_secs(),
            req.command
        ),
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
    async fn passthrough_runs_in_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let req = SandboxRequest {
            command: "pwd".into(),
            cwd: cwd.clone(),
            timeout: Duration::from_secs(5),
            policy: SandboxPolicy::DangerFullAccess,
        };
        let out = PassthroughRunner.run(req).await.unwrap();
        assert!(out.stdout.contains(&cwd.display().to_string()));
    }
}
