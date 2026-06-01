//! `grep` — ripgrep-backed content search, with grep/find fallback.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use super::support::glob::glob_to_regex;
use super::support::paths::{display_relative, resolve_in_cwd};
use super::support::truncate::{TRUNCATION_MARKER, cap_head};
use super::{
    CancelFlag, Tool, ToolError, ToolOutput, arg_opt_bool, arg_opt_str, arg_opt_u64, arg_str,
};

const DEFAULT_LIMIT: usize = 100;
const DEFAULT_MAX_LINE_LENGTH: usize = 500;
const MAX_CONTEXT_LINES: usize = 20;
const MAX_LINE_LENGTH_LIMIT: usize = 10_000;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const EXCLUDED_DIRS: &[&str] = &[".git", "node_modules", "target", ".venv", "__pycache__"];
const EXCLUDED_GLOBS: &[&str] = &[
    "!**/.git/**",
    "!**/node_modules/**",
    "!**/target/**",
    "!**/.venv/**",
    "!**/__pycache__/**",
];

pub struct GrepTool;

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents for a pattern using ripgrep. Returns matching \
         lines with file paths and line numbers. Respects .gitignore when rg \
         is available, falls back to grep/find when it is not, and supports \
         content, files_with_matches, and count output modes. Output is \
         truncated to 100 results or 50KB by default."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Search file contents with ripgrep (respects .gitignore when rg is available)")
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Search pattern (regex, or literal when literal=true)" },
                "path": { "type": "string", "description": "Directory or file to search (default: current directory)" },
                "glob": { "type": "string", "description": "Filter files by glob, e.g. '*.rs' or '**/*.spec.ts'" },
                "ignoreCase": { "type": "boolean", "description": "Case-insensitive search (default: false)" },
                "literal": { "type": "boolean", "description": "Treat pattern as a literal string instead of regex (default: false)" },
                "context": { "type": "integer", "description": "Lines of context to show before and after each match in content mode (default: 0)" },
                "limit": { "type": "integer", "description": "Maximum number of returned results (default: 100)" },
                "offset": { "type": "integer", "description": "Number of results to skip before returning output (default: 0)" },
                "outputMode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "Result shape: content lines, matching files, or match counts per file (default: content)"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "Alias for outputMode"
                },
                "maxLineLength": { "type": "integer", "description": "Maximum characters per returned content line (default: 500)" },
                "max_line_length": { "type": "integer", "description": "Alias for maxLineLength" }
            },
            "required": ["pattern"]
        })
    }

    fn execute(
        &self,
        args: &Value,
        cwd: &Path,
        cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError> {
        let request = GrepRequest::from_args(args, cwd)?;
        let run = match run_rg(cwd, &request, cancel) {
            Ok(run) => run,
            Err(RunError::MissingRipgrep) => run_grep_fallback(cwd, &request, cancel)?,
            Err(RunError::Tool(error)) => return Err(error),
        };
        Ok(ToolOutput::new(format_run(run)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputMode {
    Content,
    FilesWithMatches,
    Count,
}

struct GrepRequest<'a> {
    pattern: &'a str,
    root: PathBuf,
    glob: Option<&'a str>,
    ignore_case: bool,
    literal: bool,
    context: usize,
    limit: usize,
    offset: usize,
    output_mode: OutputMode,
    max_line_length: usize,
}

impl<'a> GrepRequest<'a> {
    fn from_args(args: &'a Value, cwd: &Path) -> Result<Self, ToolError> {
        let pattern = arg_str(args, "pattern")?;
        let base = arg_opt_str(args, "path").unwrap_or(".");
        let root = resolve_in_cwd(cwd, base)?;
        if !root.exists() {
            return Err(ToolError::new(format!("path not found: {base}")));
        }

        let limit = arg_opt_u64(args, "limit")
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_LIMIT);
        if limit == 0 {
            return Err(ToolError::new("limit must be >= 1"));
        }

        let context = arg_opt_u64(args, "context")
            .map(|value| (value as usize).min(MAX_CONTEXT_LINES))
            .unwrap_or(0);
        let offset = arg_opt_u64(args, "offset")
            .map(|value| value as usize)
            .unwrap_or(0);
        let max_line_length = arg_opt_u64(args, "maxLineLength")
            .or_else(|| arg_opt_u64(args, "max_line_length"))
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_MAX_LINE_LENGTH);
        if max_line_length == 0 {
            return Err(ToolError::new("maxLineLength must be >= 1"));
        }

        Ok(Self {
            pattern,
            root,
            glob: arg_opt_str(args, "glob"),
            ignore_case: arg_opt_bool(args, "ignoreCase"),
            literal: arg_opt_bool(args, "literal"),
            context,
            limit,
            offset,
            output_mode: parse_output_mode(args)?,
            max_line_length: max_line_length.min(MAX_LINE_LENGTH_LIMIT),
        })
    }
}

fn parse_output_mode(args: &Value) -> Result<OutputMode, ToolError> {
    let value = arg_opt_str(args, "outputMode")
        .or_else(|| arg_opt_str(args, "output_mode"))
        .unwrap_or("content");
    match value {
        "content" => Ok(OutputMode::Content),
        "files" | "files_with_matches" | "filesWithMatches" => Ok(OutputMode::FilesWithMatches),
        "count" => Ok(OutputMode::Count),
        other => Err(ToolError::new(format!(
            "invalid outputMode {other:?}; expected content, files_with_matches, or count"
        ))),
    }
}

struct SearchRun {
    text: String,
    limit_reached: bool,
}

enum RunError {
    MissingRipgrep,
    Tool(ToolError),
}

impl From<ToolError> for RunError {
    fn from(error: ToolError) -> Self {
        Self::Tool(error)
    }
}

fn run_rg(cwd: &Path, request: &GrepRequest, cancel: &CancelFlag) -> Result<SearchRun, RunError> {
    let mut child = match build_rg_command(cwd, request).spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == ErrorKind::NotFound => return Err(RunError::MissingRipgrep),
        Err(error) => {
            return Err(ToolError::new(format!("could not start ripgrep (rg): {error}")).into());
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::new("could not capture ripgrep stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::new("could not capture ripgrep stderr"))?;

    let (line_tx, line_rx) = mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let stderr_reader = thread::spawn(move || drain_string(stderr));

    let mut output = SearchOutput::new(cwd, request);
    let mut limit_reached = false;
    let mut cancelled = false;
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            cancelled = true;
            break child
                .wait()
                .map_err(|error| ToolError::new(format!("error waiting for ripgrep: {error}")))?;
        }

        if !limit_reached {
            match line_rx.recv_timeout(POLL_INTERVAL) {
                Ok(line) => {
                    if output.push_rg_json_line(&line)? {
                        limit_reached = true;
                        let _ = child.kill();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {}
            }
            while let Ok(line) = line_rx.try_recv() {
                if output.push_rg_json_line(&line)? {
                    limit_reached = true;
                    let _ = child.kill();
                    break;
                }
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                return Err(ToolError::new(format!("error waiting for ripgrep: {error}")).into());
            }
        }
    };

    if !limit_reached {
        for line in line_rx.try_iter() {
            if output.push_rg_json_line(&line)? {
                limit_reached = true;
                break;
            }
        }
    }
    let _ = stdout_reader.join();
    let stderr = stderr_reader.join().unwrap_or_default();

    if cancelled {
        return Err(ToolError::new("cancelled").into());
    }
    validate_status("ripgrep", status, limit_reached, &stderr)?;

    let mut run = output.finish();
    run.limit_reached = limit_reached || run.limit_reached;
    Ok(run)
}

fn run_grep_fallback(
    cwd: &Path,
    request: &GrepRequest,
    cancel: &CancelFlag,
) -> Result<SearchRun, ToolError> {
    let files = find_fallback_files(request, cancel)?;
    let mut output = SearchOutput::new(cwd, request);
    let mut limit_reached = false;

    for file in files {
        if cancel.load(Ordering::Relaxed) {
            return Err(ToolError::new("cancelled"));
        }

        let captured = run_capture(build_grep_command(request, &file), cancel, "grep")?;
        if captured.status.code() == Some(1) {
            continue;
        }
        validate_status("grep", captured.status, false, &captured.stderr)?;

        for line in String::from_utf8_lossy(&captured.stdout).lines() {
            let Some((line_number, content)) = parse_grep_line(line) else {
                continue;
            };
            if output.push_match(file.clone(), line_number, content) {
                limit_reached = true;
                break;
            }
        }

        if limit_reached {
            break;
        }
    }

    let mut run = output.finish();
    run.limit_reached = limit_reached || run.limit_reached;
    Ok(run)
}

fn find_fallback_files(
    request: &GrepRequest,
    cancel: &CancelFlag,
) -> Result<Vec<PathBuf>, ToolError> {
    if request.root.is_file() {
        return Ok(vec![request.root.clone()]);
    }

    let captured = run_capture(build_find_command(&request.root), cancel, "find")?;
    validate_status("find", captured.status, false, &captured.stderr)?;

    let glob = request.glob.map(glob_to_regex).transpose()?;
    let mut files = captured
        .stdout
        .split(|byte| *byte == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| PathBuf::from(String::from_utf8_lossy(chunk).into_owned()))
        .filter(|path| {
            glob.as_ref()
                .map(|glob| glob.is_match(&display_relative(&request.root, path)))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn build_rg_command(cwd: &Path, request: &GrepRequest) -> Command {
    let mut command = Command::new("rg");
    command
        .arg("--no-config")
        .arg("--json")
        .arg("--line-number")
        .arg("--color=never")
        .arg("--hidden")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if request.ignore_case {
        command.arg("--ignore-case");
    }
    if request.literal {
        command.arg("--fixed-strings");
    }
    if let Some(glob) = request.glob {
        command.arg("--glob").arg(glob);
    }
    for glob in EXCLUDED_GLOBS {
        command.arg("--glob").arg(glob);
    }
    command.arg("--").arg(request.pattern).arg(&request.root);
    command
}

fn build_grep_command(request: &GrepRequest, file: &Path) -> Command {
    let mut command = Command::new("grep");
    command.arg("-n").arg("-I");
    if request.ignore_case {
        command.arg("-i");
    }
    if request.literal {
        command.arg("-F");
    } else {
        command.arg("-E");
    }
    command.arg("--").arg(request.pattern).arg(file);
    command
}

fn build_find_command(root: &Path) -> Command {
    let mut command = Command::new("find");
    command.arg(root).arg("(");
    for (index, dir) in EXCLUDED_DIRS.iter().enumerate() {
        if index > 0 {
            command.arg("-o");
        }
        command.arg("-name").arg(dir);
    }
    command
        .arg(")")
        .arg("-prune")
        .arg("-o")
        .arg("-type")
        .arg("f")
        .arg("-print0");
    command
}

struct SearchOutput<'a> {
    cwd: &'a Path,
    root: &'a Path,
    is_directory: bool,
    mode: OutputMode,
    context: usize,
    limit: usize,
    offset: usize,
    max_line_length: usize,
    seen_matches: usize,
    returned: usize,
    text: String,
    seen_files: HashSet<PathBuf>,
    file_counts: HashMap<PathBuf, usize>,
    file_count_order: Vec<PathBuf>,
    file_cache: HashMap<PathBuf, Vec<String>>,
}

impl<'a> SearchOutput<'a> {
    fn new(cwd: &'a Path, request: &'a GrepRequest) -> Self {
        Self {
            cwd,
            root: &request.root,
            is_directory: request.root.is_dir(),
            mode: request.output_mode,
            context: request.context,
            limit: request.limit,
            offset: request.offset,
            max_line_length: request.max_line_length,
            seen_matches: 0,
            returned: 0,
            text: String::new(),
            seen_files: HashSet::new(),
            file_counts: HashMap::new(),
            file_count_order: Vec::new(),
            file_cache: HashMap::new(),
        }
    }

    /// Returns `true` when the current output mode has enough results.
    fn push_rg_json_line(&mut self, line: &str) -> Result<bool, ToolError> {
        let event: Value = serde_json::from_str(line)
            .map_err(|error| ToolError::new(format!("invalid ripgrep JSON: {error}")))?;
        if event.get("type").and_then(Value::as_str) != Some("match") {
            return Ok(false);
        }

        let Some(data) = event.get("data") else {
            return Ok(false);
        };
        let Some(path_text) = data
            .get("path")
            .and_then(|path| path.get("text"))
            .and_then(Value::as_str)
        else {
            return Ok(false);
        };
        let Some(line_number) = data.get("line_number").and_then(Value::as_u64) else {
            return Ok(false);
        };
        let Some(line_text) = data
            .get("lines")
            .and_then(|lines| lines.get("text"))
            .and_then(Value::as_str)
        else {
            return Ok(false);
        };

        Ok(self.push_match(
            absolute_path(self.cwd, path_text),
            line_number as usize,
            trim_line_end(line_text),
        ))
    }

    fn push_match(&mut self, path: PathBuf, line_number: usize, line_text: &str) -> bool {
        self.seen_matches += 1;

        match self.mode {
            OutputMode::Content => {
                if self.seen_matches <= self.offset {
                    return false;
                }
                if self.returned >= self.limit {
                    return true;
                }
                self.push_content_match(&path, line_number, line_text);
                self.returned += 1;
                self.returned >= self.limit
            }
            OutputMode::FilesWithMatches => {
                if !self.seen_files.insert(path.clone()) {
                    return false;
                }
                let seen_file_count = self.seen_files.len();
                if seen_file_count <= self.offset {
                    return false;
                }
                if self.returned >= self.limit {
                    return true;
                }
                self.text.push_str(&format!(
                    "{}\n",
                    label_for(self.root, &path, self.is_directory)
                ));
                self.returned += 1;
                self.returned >= self.limit
            }
            OutputMode::Count => {
                if !self.file_counts.contains_key(&path) {
                    self.file_count_order.push(path.clone());
                }
                *self.file_counts.entry(path).or_insert(0) += 1;
                false
            }
        }
    }

    fn push_content_match(&mut self, path: &Path, line_number: usize, fallback_line: &str) {
        let label = label_for(self.root, path, self.is_directory);
        let max_line_length = self.max_line_length;
        if self.context == 0 {
            self.text.push_str(&format!(
                "{label}:{line_number}:{}\n",
                clip(fallback_line, max_line_length)
            ));
            return;
        }

        let context = self.context;
        let block = {
            let lines = self.lines_for(path);
            if lines.is_empty() || line_number == 0 {
                format!(
                    "{label}:{line_number}:{}\n",
                    clip(fallback_line, max_line_length)
                )
            } else {
                let index = line_number - 1;
                let start = index.saturating_sub(context);
                let end = (index + context + 1).min(lines.len());
                let mut block = String::new();
                for (offset, line) in lines[start..end].iter().enumerate() {
                    let current = start + offset + 1;
                    let separator = if current == line_number { ':' } else { '-' };
                    block.push_str(&format!(
                        "{label}{separator}{current}{separator}{}\n",
                        clip(line, max_line_length)
                    ));
                }
                block
            }
        };
        self.text.push_str(&block);
    }

    fn lines_for(&mut self, path: &Path) -> &[String] {
        if !self.file_cache.contains_key(path) {
            let lines: Vec<String> = fs::read_to_string(path)
                .map(|content| {
                    content
                        .replace("\r\n", "\n")
                        .replace('\r', "\n")
                        .split('\n')
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            self.file_cache.insert(path.to_path_buf(), lines);
        }

        self.file_cache.get(path).map(Vec::as_slice).unwrap_or(&[])
    }

    fn finish(mut self) -> SearchRun {
        if self.mode == OutputMode::Count {
            let total_files = self.file_count_order.len();
            for path in self
                .file_count_order
                .iter()
                .skip(self.offset)
                .take(self.limit)
            {
                let count = self.file_counts.get(path).copied().unwrap_or(0);
                self.text.push_str(&format!(
                    "{}:{count}\n",
                    label_for(self.root, path, self.is_directory)
                ));
            }
            return SearchRun {
                text: self.text,
                limit_reached: total_files > self.offset.saturating_add(self.limit),
            };
        }

        SearchRun {
            text: self.text,
            limit_reached: false,
        }
    }
}

fn format_run(run: SearchRun) -> String {
    if run.text.is_empty() {
        return "No matches.".to_owned();
    }

    let mut capped = cap_head(&run.text);
    if run.limit_reached && !capped.ends_with(TRUNCATION_MARKER) {
        capped.push_str(TRUNCATION_MARKER);
    }
    capped
}

fn validate_status(
    command: &str,
    status: ExitStatus,
    limit_reached: bool,
    stderr: &str,
) -> Result<(), ToolError> {
    if status.success() || status.code() == Some(1) || limit_reached {
        return Ok(());
    }
    let message = if stderr.trim().is_empty() {
        format!("{command} failed with status {status}")
    } else {
        stderr.trim().to_owned()
    };
    Err(ToolError::new(message))
}

struct CapturedCommand {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: String,
}

fn run_capture(
    mut command: Command,
    cancel: &CancelFlag,
    command_name: &str,
) -> Result<CapturedCommand, ToolError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ToolError::new(format!("could not start {command_name}: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::new(format!("could not capture {command_name} stdout")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::new(format!("could not capture {command_name} stderr")))?;

    let stdout_reader = thread::spawn(move || drain_bytes(stdout));
    let stderr_reader = thread::spawn(move || drain_string(stderr));

    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ToolError::new("cancelled"));
        }

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(error) => {
                let _ = child.kill();
                return Err(ToolError::new(format!(
                    "error waiting for {command_name}: {error}"
                )));
            }
        }
    };

    Ok(CapturedCommand {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

fn parse_grep_line(line: &str) -> Option<(usize, &str)> {
    let (line_number, content) = line.split_once(':')?;
    Some((line_number.parse().ok()?, content))
}

fn absolute_path(cwd: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn label_for(root: &Path, path: &Path, is_directory: bool) -> String {
    if is_directory {
        return display_relative(root, path);
    }
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn trim_line_end(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

fn clip(line: &str, max_len: usize) -> String {
    if line.len() <= max_len {
        return line.to_owned();
    }
    let mut end = max_len;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

fn drain_string(mut pipe: impl Read) -> String {
    let mut buffer = String::new();
    let _ = pipe.read_to_string(&mut buffer);
    buffer
}

fn drain_bytes(mut pipe: impl Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = pipe.read_to_end(&mut buffer);
    buffer
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use super::*;

    #[test]
    fn fallback_uses_find_and_grep_for_counts() {
        let root = std::env::temp_dir().join(format!("nav_grep_{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(root.join("a.txt"), "needle one\nneedle two\n").expect("write a");
        fs::write(root.join("b.txt"), "needle three\n").expect("write b");

        let request = GrepRequest {
            pattern: "needle",
            root: root.clone(),
            glob: None,
            ignore_case: false,
            literal: true,
            context: 0,
            limit: 100,
            offset: 0,
            output_mode: OutputMode::Count,
            max_line_length: DEFAULT_MAX_LINE_LENGTH,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let run = run_grep_fallback(&root, &request, &cancel).expect("fallback search");

        assert!(run.text.contains("a.txt:2"), "{}", run.text);
        assert!(run.text.contains("b.txt:1"), "{}", run.text);

        let _ = fs::remove_dir_all(root);
    }
}
