use std::process::Stdio;
use std::time::Instant;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time;

use super::AgentEvent;
use super::runner::{SessionBinding, emit};
use crate::context::{ExtensionCatalog, ExtensionHook, HookCommand, HookEventType};
use crate::tool_registry::truncate::MAX_BYTES;

const HOOK_OUTPUT_MAX_BYTES: usize = MAX_BYTES;

pub(super) async fn run_hooks(
    catalog: Option<&ExtensionCatalog>,
    event_type: HookEventType,
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
) -> bool {
    let Some(catalog) = catalog else {
        return false;
    };
    let hooks: Vec<&ExtensionHook> = catalog
        .hooks()
        .iter()
        .filter(|hook| hook.event_type == event_type)
        .collect();
    for hook in &hooks {
        run_hook(hook, events, session).await;
    }
    !hooks.is_empty()
}

async fn run_hook(
    hook: &ExtensionHook,
    events: &UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
) {
    emit(
        events,
        session,
        AgentEvent::HookStarted {
            name: hook.name.clone(),
            event_type: hook.event_type.as_str().to_string(),
        },
    );

    let started_at = Instant::now();
    let output = spawn_hook(hook).await;
    let duration_ms = started_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let (stdout, stderr, exit_status, success) = match output {
        HookProcessResult::Completed(output) => (
            output.stdout,
            output.stderr,
            output.exit_status,
            output.success,
        ),
        HookProcessResult::TimedOut => (
            String::new(),
            format!("hook timed out after {}ms", hook.timeout.as_millis()),
            None,
            false,
        ),
        HookProcessResult::SpawnFailed(err) => (String::new(), err, None, false),
    };

    emit(
        events,
        session,
        AgentEvent::HookCompleted {
            name: hook.name.clone(),
            event_type: hook.event_type.as_str().to_string(),
            duration_ms,
            stdout,
            stderr,
            exit_status,
            success,
        },
    );
}

enum HookProcessResult {
    Completed(HookOutput),
    TimedOut,
    SpawnFailed(String),
}

struct HookOutput {
    stdout: String,
    stderr: String,
    exit_status: Option<i32>,
    success: bool,
}

async fn spawn_hook(hook: &ExtensionHook) -> HookProcessResult {
    let mut command = match hook_command(&hook.command) {
        Ok(command) => command,
        Err(err) => return HookProcessResult::SpawnFailed(err),
    };
    prepare_timeout_cleanup(&mut command);
    command
        .current_dir(&hook.extension_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return HookProcessResult::SpawnFailed(format!("failed to spawn hook: {err}")),
    };

    let child_pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let wait_for_hook = async move {
        let stdout = read_capped_output(stdout);
        let stderr = read_capped_output(stderr);
        let status = child.wait();
        let (status, stdout, stderr) = tokio::join!(status, stdout, stderr);
        status.map(|status| HookOutput {
            stdout,
            stderr,
            exit_status: status.code(),
            success: status.success(),
        })
    };
    match time::timeout(hook.timeout, wait_for_hook).await {
        Ok(Ok(output)) => HookProcessResult::Completed(output),
        Ok(Err(err)) => HookProcessResult::SpawnFailed(format!("failed to wait for hook: {err}")),
        Err(_) => {
            cleanup_timed_out_hook(child_pid);
            HookProcessResult::TimedOut
        }
    }
}

async fn read_capped_output<R>(reader: Option<R>) -> String
where
    R: AsyncRead + Unpin,
{
    let Some(mut reader) = reader else {
        return String::new();
    };
    let mut retained = Vec::new();
    let mut total_bytes = 0usize;
    let mut chunk = [0u8; 8192];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) => return format!("failed to read hook output: {err}"),
        };
        total_bytes = total_bytes.saturating_add(read);
        let remaining = HOOK_OUTPUT_MAX_BYTES.saturating_sub(retained.len());
        if remaining > 0 {
            retained.extend_from_slice(&chunk[..read.min(remaining)]);
        }
    }
    let mut output = String::from_utf8_lossy(&retained).into_owned();
    if total_bytes > retained.len() {
        output.push_str(&format!(
            "\n[truncated {} bytes]\n",
            total_bytes - retained.len()
        ));
    }
    output
}

fn hook_command(command: &HookCommand) -> Result<Command, String> {
    match command {
        HookCommand::Shell(command) => Ok(shell_command(command)),
        HookCommand::Argv(argv) => {
            let Some(program) = argv.first() else {
                return Err("hook command array must not be empty".to_string());
            };
            let mut command = Command::new(program);
            command.args(&argv[1..]);
            Ok(command)
        }
        HookCommand::Path(path) => Ok(Command::new(path)),
    }
}

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

#[cfg(unix)]
fn prepare_timeout_cleanup(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn prepare_timeout_cleanup(_command: &mut Command) {}

#[cfg(unix)]
fn cleanup_timed_out_hook(child_pid: Option<u32>) {
    let Some(child_pid) = child_pid else {
        return;
    };
    let group = format!("-{child_pid}");
    let _ = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(&group)
        .status();
    let _ = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(group)
        .status();
}

#[cfg(not(unix))]
fn cleanup_timed_out_hook(_child_pid: Option<u32>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExtensionScope, HookCommand};
    use std::fs;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    fn hook(command: HookCommand, timeout: Duration) -> ExtensionHook {
        let dir = tempdir().unwrap().keep();
        ExtensionHook {
            name: "demo-hook".into(),
            extension_name: "demo".into(),
            extension_dir: dir,
            scope: ExtensionScope::Project,
            event_type: HookEventType::PreTurn,
            command,
            timeout,
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn run_hook_emits_started_and_completed_with_output() {
        let hook = hook(
            HookCommand::Shell("printf stdout && printf stderr >&2".into()),
            Duration::from_secs(5),
        );
        let catalog = ExtensionCatalog::with_hooks(vec![], vec![], vec![], vec![hook]);
        let (tx, mut rx) = mpsc::unbounded_channel();

        run_hooks(Some(&catalog), HookEventType::PreTurn, &tx, None).await;

        assert!(matches!(
            rx.recv().await,
            Some(AgentEvent::HookStarted { .. })
        ));
        match rx.recv().await {
            Some(AgentEvent::HookCompleted {
                stdout,
                stderr,
                success,
                ..
            }) => {
                assert_eq!(stdout, "stdout");
                assert_eq!(stderr, "stderr");
                assert!(success);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn run_hook_marks_nonzero_exit_unsuccessful() {
        let hook = hook(
            HookCommand::Shell("printf nope >&2; exit 7".into()),
            Duration::from_secs(5),
        );
        let catalog = ExtensionCatalog::with_hooks(vec![], vec![], vec![], vec![hook]);
        let (tx, mut rx) = mpsc::unbounded_channel();

        run_hooks(Some(&catalog), HookEventType::PreTurn, &tx, None).await;

        let _ = rx.recv().await;
        match rx.recv().await {
            Some(AgentEvent::HookCompleted {
                stderr, success, ..
            }) => {
                assert_eq!(stderr, "nope");
                assert!(!success);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn run_hook_timeout_completes_unsuccessfully() {
        let hook = hook(
            HookCommand::Shell("sleep 2".into()),
            Duration::from_millis(10),
        );
        let catalog = ExtensionCatalog::with_hooks(vec![], vec![], vec![], vec![hook]);
        let (tx, mut rx) = mpsc::unbounded_channel();

        run_hooks(Some(&catalog), HookEventType::PreTurn, &tx, None).await;

        let _ = rx.recv().await;
        match rx.recv().await {
            Some(AgentEvent::HookCompleted {
                stderr, success, ..
            }) => {
                assert!(stderr.contains("timed out"));
                assert!(!success);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn run_hook_bounds_noisy_output() {
        let hook = hook(
            HookCommand::Shell("yes noisy | head -n 20000".into()),
            Duration::from_secs(5),
        );
        let catalog = ExtensionCatalog::with_hooks(vec![], vec![], vec![], vec![hook]);
        let (tx, mut rx) = mpsc::unbounded_channel();

        run_hooks(Some(&catalog), HookEventType::PreTurn, &tx, None).await;

        let _ = rx.recv().await;
        match rx.recv().await {
            Some(AgentEvent::HookCompleted { stdout, .. }) => {
                assert!(stdout.contains("[truncated"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn run_hook_executes_extension_local_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let script = dir.path().join("hook.sh");
        fs::write(&script, "#!/bin/sh\nprintf path-hook\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        let hook = ExtensionHook {
            name: "path-hook".into(),
            extension_name: "demo".into(),
            extension_dir: dir.path().to_path_buf(),
            scope: ExtensionScope::Project,
            event_type: HookEventType::PreTurn,
            command: HookCommand::Path(script),
            timeout: Duration::from_secs(5),
        };
        let catalog = ExtensionCatalog::with_hooks(vec![], vec![], vec![], vec![hook]);
        let (tx, mut rx) = mpsc::unbounded_channel();

        run_hooks(Some(&catalog), HookEventType::PreTurn, &tx, None).await;

        let _ = rx.recv().await;
        match rx.recv().await {
            Some(AgentEvent::HookCompleted {
                stdout, success, ..
            }) => {
                assert_eq!(stdout, "path-hook");
                assert!(success);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
