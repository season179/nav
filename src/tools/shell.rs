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
