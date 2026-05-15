use anyhow::{Context, Result, bail};
use std::{path::Path, process::Stdio};
use tokio::{process::Command, time};

pub(super) async fn bash(cwd: &Path, timeout_secs: u64, command: &str) -> Result<String> {
    // shell access is powerful and risky. We run in the workspace, capture
    // stdout/stderr for the model, and enforce a timeout so commands cannot hang.
    let child = Command::new("sh")
        .kill_on_drop(true)
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn command `{command}`"))?;

    let output = match time::timeout(
        time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(output) => output?,
        Err(_) => bail!("command timed out after {timeout_secs}s: {command}"),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status, stdout, stderr
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[tokio::test]
    async fn bash_captures_stdout() {
        let result = bash(Path::new("/tmp"), 5, "echo hello").await.unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("status:"));
    }

    #[tokio::test]
    async fn bash_captures_stderr() {
        let result = bash(Path::new("/tmp"), 5, "echo oops >&2").await.unwrap();
        assert!(result.contains("stderr:\noops"));
    }

    #[tokio::test]
    async fn bash_reports_exit_status() {
        // On Unix, ExitStatus Display is "exit status: N".
        let result = bash(Path::new("/tmp"), 5, "exit 42").await.unwrap();
        assert!(result.contains("status: exit status: 42"));
    }

    #[tokio::test]
    async fn bash_reports_zero_exit() {
        let result = bash(Path::new("/tmp"), 5, "true").await.unwrap();
        assert!(result.contains("status: exit status: 0"));
    }

    #[tokio::test]
    async fn bash_timeout_returns_error() {
        let result = bash(Path::new("/tmp"), 1, "sleep 60").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn bash_runs_in_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let result = bash(&cwd, 5, "pwd").await.unwrap();
        assert!(result.contains(&format!("stdout:\n{}", cwd.display())));
    }
}
