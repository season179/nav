use anyhow::Result;
use std::path::Path;
use std::time::Duration;

use crate::sandbox::SandboxRequest;
use crate::tools::preflight::PermissionContext;

pub(super) async fn bash(
    permissions: &PermissionContext,
    cwd: &Path,
    timeout_secs: u64,
    command: &str,
) -> Result<String> {
    // shell access is powerful and risky. The classifier already gated
    // dangerous commands in `preflight`; here we just spawn under the
    // sandbox runner chosen for the active policy.
    let req = SandboxRequest {
        command: command.to_string(),
        cwd: cwd.to_path_buf(),
        timeout: Duration::from_secs(timeout_secs),
        policy: permissions.sandbox_policy.clone(),
    };
    let output = permissions.sandbox.run(req).await?;
    Ok(format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status_display, output.stdout, output.stderr
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::unchecked_permission_context;
    use std::path::Path;

    #[tokio::test]
    async fn bash_captures_stdout() {
        let result = bash(
            &unchecked_permission_context(),
            Path::new("/tmp"),
            5,
            "echo hello",
        )
        .await
        .unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("status:"));
    }

    #[tokio::test]
    async fn bash_captures_stderr() {
        let result = bash(
            &unchecked_permission_context(),
            Path::new("/tmp"),
            5,
            "echo oops >&2",
        )
        .await
        .unwrap();
        assert!(result.contains("stderr:\noops"));
    }

    #[tokio::test]
    async fn bash_reports_exit_status() {
        let result = bash(
            &unchecked_permission_context(),
            Path::new("/tmp"),
            5,
            "exit 42",
        )
        .await
        .unwrap();
        assert!(result.contains("status: exit status: 42"));
    }

    #[tokio::test]
    async fn bash_reports_zero_exit() {
        let result = bash(
            &unchecked_permission_context(),
            Path::new("/tmp"),
            5,
            "true",
        )
        .await
        .unwrap();
        assert!(result.contains("status: exit status: 0"));
    }

    #[tokio::test]
    async fn bash_timeout_returns_error() {
        let result = bash(
            &unchecked_permission_context(),
            Path::new("/tmp"),
            1,
            "sleep 60",
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn bash_runs_in_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let result = bash(&unchecked_permission_context(), &cwd, 5, "pwd")
            .await
            .unwrap();
        assert!(result.contains(&format!("stdout:\n{}", cwd.display())));
    }
}
