//! Shell process execution for workspace tools.
//!
//! Commands run in the session cwd with a deliberately small inherited
//! environment: `PATH`, `HOME`, `USER`, `LANG`, and `TERM`. The shell may add
//! its own process-local variables such as `PWD`, `SHLVL`, and `_`, but arbitrary
//! parent-process variables are not forwarded.

use std::env;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::process::Command;
use tokio::task::{JoinError, JoinHandle};

const SHELL_PATH: &str = "/bin/bash";
const ALLOWED_ENV_VARS: [&str; 5] = ["PATH", "HOME", "USER", "LANG", "TERM"];

#[derive(Debug, Clone)]
pub struct ShellCommand {
    pub command: String,
    pub cwd: PathBuf,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOutput {
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub termination: ShellTermination,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellTermination {
    Exited,
    TimedOut,
    Cancelled,
}

pub async fn run_shell_command(command: ShellCommand) -> Result<ShellOutput, ShellError> {
    run_shell_command_until(command, std::future::pending()).await
}

pub async fn run_shell_command_until<F>(
    command: ShellCommand,
    cancelled: F,
) -> Result<ShellOutput, ShellError>
where
    F: Future<Output = ()>,
{
    let mut process = shell_command(&command.command, &command.cwd);
    configure_process_group(&mut process);
    let mut child = process.spawn().map_err(ShellError::Spawn)?;
    let process_id = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or(ShellError::PipeUnavailable("stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or(ShellError::PipeUnavailable("stderr"))?;
    let stdout_task = tokio::spawn(read_pipe(stdout));
    let stderr_task = tokio::spawn(read_pipe(stderr));

    let wait = wait_for_child(&mut child, command.timeout, cancelled).await?;
    let (status, termination) = match wait {
        ChildWait::Exited(status) => (status, ShellTermination::Exited),
        ChildWait::TimedOut => {
            terminate_child(&mut child, process_id, ShellTermination::TimedOut).await?
        }
        ChildWait::Cancelled => {
            terminate_child(&mut child, process_id, ShellTermination::Cancelled).await?
        }
    };
    let stdout = read_task_output(stdout_task).await?;
    let stderr = read_task_output(stderr_task).await?;

    Ok(ShellOutput {
        status_code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).to_string(),
        stderr: String::from_utf8_lossy(&stderr).to_string(),
        termination,
    })
}

async fn terminate_child(
    child: &mut Child,
    process_id: Option<u32>,
    termination: ShellTermination,
) -> Result<(ExitStatus, ShellTermination), ShellError> {
    kill_process_group(process_id)?;
    Ok((child.wait().await.map_err(ShellError::Wait)?, termination))
}

fn shell_command(command: &str, cwd: &Path) -> Command {
    let mut child = Command::new(SHELL_PATH);
    child
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    for name in ALLOWED_ENV_VARS {
        if let Some(value) = env::var_os(name) {
            child.env(name, value);
        }
    }

    child
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

async fn wait_for_child<F>(
    child: &mut Child,
    timeout: Option<Duration>,
    cancelled: F,
) -> Result<ChildWait, ShellError>
where
    F: Future<Output = ()>,
{
    tokio::pin!(cancelled);

    if let Some(timeout) = timeout {
        let sleep = tokio::time::sleep(timeout);
        tokio::pin!(sleep);
        return tokio::select! {
            status = child.wait() => status.map(ChildWait::Exited).map_err(ShellError::Wait),
            _ = &mut sleep => Ok(ChildWait::TimedOut),
            _ = &mut cancelled => Ok(ChildWait::Cancelled),
        };
    }

    tokio::select! {
        status = child.wait() => status.map(ChildWait::Exited).map_err(ShellError::Wait),
        _ = &mut cancelled => Ok(ChildWait::Cancelled),
    }
}

#[derive(Debug)]
enum ChildWait {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

async fn read_pipe<P>(mut pipe: P) -> io::Result<Vec<u8>>
where
    P: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

async fn read_task_output(task: JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, ShellError> {
    task.await
        .map_err(ShellError::OutputTask)?
        .map_err(ShellError::ReadOutput)
}

#[cfg(unix)]
fn kill_process_group(process_id: Option<u32>) -> Result<(), ShellError> {
    let Some(process_id) = process_id else {
        return Ok(());
    };

    let process_group_id = -(process_id as libc::pid_t);
    if unsafe { libc::kill(process_group_id, libc::SIGKILL) } == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(ShellError::Kill(error))
    }
}

#[cfg(not(unix))]
fn kill_process_group(_process_id: Option<u32>) -> Result<(), ShellError> {
    Ok(())
}

#[derive(Debug)]
pub enum ShellError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Kill(std::io::Error),
    PipeUnavailable(&'static str),
    ReadOutput(std::io::Error),
    OutputTask(JoinError),
}

impl fmt::Display for ShellError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "failed to spawn shell command: {error}"),
            Self::Wait(error) => write!(formatter, "failed to wait for shell command: {error}"),
            Self::Kill(error) => write!(formatter, "failed to kill shell process group: {error}"),
            Self::PipeUnavailable(name) => {
                write!(formatter, "failed to capture shell {name} pipe")
            }
            Self::ReadOutput(error) => write!(formatter, "failed to read shell output: {error}"),
            Self::OutputTask(error) => write!(formatter, "shell output reader failed: {error}"),
        }
    }
}

impl Error for ShellError {}
