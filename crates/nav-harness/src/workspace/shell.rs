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
const OUTPUT_FLUSH_INTERVAL: Duration = Duration::from_millis(50);
const OUTPUT_MAX_EVENT_BYTES: usize = 4096;
const PIPE_READ_BUFFER_BYTES: usize = 1024;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellOutputStream {
    Stdout,
    Stderr,
}

impl ShellOutputStream {
    pub fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOutputChunk {
    pub stream: ShellOutputStream,
    pub chunk: String,
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
    run_shell_command_streaming_until(command, cancelled, |_| {}).await
}

pub async fn run_shell_command_streaming_until<F, C>(
    command: ShellCommand,
    cancelled: F,
    on_chunk: C,
) -> Result<ShellOutput, ShellError>
where
    F: Future<Output = ()>,
    C: Fn(ShellOutputChunk) + Clone + Send + Sync + 'static,
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
    let stdout_task = tokio::spawn(read_pipe(
        stdout,
        ShellOutputStream::Stdout,
        on_chunk.clone(),
    ));
    let stderr_task = tokio::spawn(read_pipe(stderr, ShellOutputStream::Stderr, on_chunk));

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

async fn read_pipe<P, C>(mut pipe: P, stream: ShellOutputStream, on_chunk: C) -> io::Result<Vec<u8>>
where
    P: AsyncRead + Unpin,
    C: Fn(ShellOutputChunk),
{
    let mut bytes = Vec::new();
    let mut pending = Vec::new();
    let mut read_buffer = [0; PIPE_READ_BUFFER_BYTES];

    loop {
        if pending.is_empty() {
            let read = pipe.read(&mut read_buffer).await?;
            if read == 0 {
                return Ok(bytes);
            }

            bytes.extend_from_slice(&read_buffer[..read]);
            pending.extend_from_slice(&read_buffer[..read]);
            flush_full_output_events(stream, &mut pending, &on_chunk);
            continue;
        }

        let flush = tokio::time::sleep(OUTPUT_FLUSH_INTERVAL);
        tokio::pin!(flush);
        tokio::select! {
            read = pipe.read(&mut read_buffer) => {
                let read = read?;
                if read == 0 {
                    flush_pending_output(stream, &mut pending, &on_chunk);
                    return Ok(bytes);
                }

                bytes.extend_from_slice(&read_buffer[..read]);
                pending.extend_from_slice(&read_buffer[..read]);
                flush_full_output_events(stream, &mut pending, &on_chunk);
            }
            _ = &mut flush => {
                flush_pending_output(stream, &mut pending, &on_chunk);
            }
        }
    }
}

fn flush_full_output_events<C>(stream: ShellOutputStream, pending: &mut Vec<u8>, on_chunk: &C)
where
    C: Fn(ShellOutputChunk),
{
    while pending.len() >= OUTPUT_MAX_EVENT_BYTES {
        let chunk = pending.drain(..OUTPUT_MAX_EVENT_BYTES).collect::<Vec<_>>();
        emit_output_chunk(stream, chunk, on_chunk);
    }
}

fn flush_pending_output<C>(stream: ShellOutputStream, pending: &mut Vec<u8>, on_chunk: &C)
where
    C: Fn(ShellOutputChunk),
{
    if pending.is_empty() {
        return;
    }

    let chunk = std::mem::take(pending);
    emit_output_chunk(stream, chunk, on_chunk);
}

fn emit_output_chunk<C>(stream: ShellOutputStream, chunk: Vec<u8>, on_chunk: &C)
where
    C: Fn(ShellOutputChunk),
{
    on_chunk(ShellOutputChunk {
        stream,
        chunk: String::from_utf8_lossy(&chunk).to_string(),
    });
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use super::{
        OUTPUT_MAX_EVENT_BYTES, ShellCommand, ShellOutputChunk, ShellOutputStream,
        run_shell_command_streaming_until,
    };

    #[tokio::test]
    async fn streaming_shell_output_flushes_time_batched_chunks() {
        let workspace = TestWorkspace::new("time_batches");
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let chunks_for_callback = Arc::clone(&chunks);

        let output = run_shell_command_streaming_until(
            ShellCommand {
                command: "printf 'first\\n'; sleep 0.15; printf 'second\\n'".to_string(),
                cwd: workspace.root.clone(),
                timeout: None,
            },
            std::future::pending(),
            move |chunk| chunks_for_callback.lock().unwrap().push(chunk),
        )
        .await
        .expect("streaming shell command should run");

        assert_eq!(output.stdout, "first\nsecond\n");
        assert_eq!(
            stdout_chunks(&chunks.lock().unwrap()),
            vec!["first\n".to_string(), "second\n".to_string()]
        );
    }

    #[tokio::test]
    async fn streaming_shell_output_splits_events_at_max_size() {
        let workspace = TestWorkspace::new("max_size");
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let chunks_for_callback = Arc::clone(&chunks);

        let output = run_shell_command_streaming_until(
            ShellCommand {
                command: "for i in {1..5000}; do printf x; done".to_string(),
                cwd: workspace.root.clone(),
                timeout: None,
            },
            std::future::pending(),
            move |chunk| chunks_for_callback.lock().unwrap().push(chunk),
        )
        .await
        .expect("streaming shell command should run");

        let chunks = chunks.lock().unwrap();
        let stdout_chunks = stdout_chunks(&chunks);
        assert_eq!(output.stdout.len(), 5000);
        assert_eq!(stdout_chunks.concat(), output.stdout);
        assert!(stdout_chunks.len() >= 2);
        assert!(
            stdout_chunks
                .iter()
                .all(|chunk| chunk.len() <= OUTPUT_MAX_EVENT_BYTES)
        );
    }

    fn stdout_chunks(chunks: &[ShellOutputChunk]) -> Vec<String> {
        chunks
            .iter()
            .filter(|chunk| chunk.stream == ShellOutputStream::Stdout)
            .map(|chunk| chunk.chunk.clone())
            .collect()
    }

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("nav-shell-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(root).expect("workspace should canonicalize"),
            }
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
