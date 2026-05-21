use anyhow::Result;
use std::path::{Component, Path};
use std::time::Duration;

use crate::guardrails::PermissionContext;
use crate::guardrails::SandboxRequest;
use crate::tool_registry::output_accumulator::{AccumulatorOutput, OutputAccumulator};
use crate::{permissions::bash_parse::parse_command_pipeline, tool_registry::fs};

use super::read_filter::{self, ReadOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReadRewrite {
    Cat {
        files: Vec<String>,
        line_numbers: bool,
    },
    Head {
        file: String,
        max_lines: Option<usize>,
    },
    Tail {
        file: String,
        tail_lines: usize,
    },
}

pub(super) async fn bash(
    permissions: &PermissionContext,
    cwd: &Path,
    timeout_secs: u64,
    command: &str,
) -> Result<AccumulatorOutput> {
    // Both the read-rewrite shortcut and the native sandbox path produce a
    // single combined string; feed it through the accumulator so the
    // model-visible output is uniformly bounded and an oversize result
    // spills to a log file under the nav data dir with a
    // `[Full output: <path>]` trailer.
    let combined = if let Some(rewrite) = read_rewrite(command) {
        run_read_rewrite(cwd, rewrite)?
    } else {
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
        format!(
            "status: {}\nstdout:\n{}\nstderr:\n{}",
            output.status_display, output.stdout, output.stderr
        )
    };
    let mut acc = OutputAccumulator::new("bash")?;
    acc.push(combined.as_bytes())?;
    acc.finish()
}

fn read_rewrite(command: &str) -> Option<ReadRewrite> {
    let pipeline = parse_command_pipeline(command).ok()?;
    if pipeline.len() != 1 {
        return None;
    }
    let argv = pipeline.first()?;
    let program = argv.first()?.as_str();
    match program {
        "cat" => cat_rewrite(&argv[1..]),
        "head" => head_rewrite(&argv[1..]),
        "tail" => tail_rewrite(&argv[1..]),
        _ => None,
    }
}

fn cat_rewrite(args: &[String]) -> Option<ReadRewrite> {
    if args.is_empty() {
        return None;
    }

    let (line_numbers, files) = if args.first().is_some_and(|arg| arg == "-n") {
        (true, &args[1..])
    } else {
        (false, args)
    };
    if files.is_empty() || !files.iter().all(|file| can_read_internally(file)) {
        return None;
    }
    if files.iter().any(|file| file.starts_with('-')) {
        return None;
    }

    Some(ReadRewrite::Cat {
        files: files.to_vec(),
        line_numbers,
    })
}

fn head_rewrite(args: &[String]) -> Option<ReadRewrite> {
    match args {
        [file] if can_read_internally(file) => Some(ReadRewrite::Head {
            file: file.clone(),
            max_lines: None,
        }),
        [flag, file] if can_read_internally(file) => {
            parse_line_count_flag(flag).map(|max_lines| ReadRewrite::Head {
                file: file.clone(),
                max_lines: Some(max_lines),
            })
        }
        _ => None,
    }
}

fn tail_rewrite(args: &[String]) -> Option<ReadRewrite> {
    match args {
        [flag, file] if can_read_internally(file) => {
            parse_line_count_flag(flag).map(|tail_lines| ReadRewrite::Tail {
                file: file.clone(),
                tail_lines,
            })
        }
        [flag, n, file] if can_read_internally(file) => {
            parse_tail_lines_split(flag, n).map(|tail_lines| ReadRewrite::Tail {
                file: file.clone(),
                tail_lines,
            })
        }
        _ => None,
    }
}

fn parse_line_count_flag(flag: &str) -> Option<usize> {
    let count = if let Some(count) = flag.strip_prefix("--lines=") {
        count
    } else {
        flag.strip_prefix('-')?
    };
    parse_ascii_usize(count)
}

fn parse_tail_lines_split(flag: &str, n: &str) -> Option<usize> {
    if flag != "-n" && flag != "--lines" {
        return None;
    }
    parse_ascii_usize(n)
}

fn parse_ascii_usize(value: &str) -> Option<usize> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn can_read_internally(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        && !path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|b| matches!(b, b'*' | b'?' | b'[' | b']' | b'$' | b'{' | b'}'))
        && !path.starts_with("~")
}

fn run_read_rewrite(cwd: &Path, rewrite: ReadRewrite) -> Result<String> {
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut status = 0;

    match rewrite {
        ReadRewrite::Cat {
            files,
            line_numbers,
        } => {
            let options = ReadOptions {
                line_numbers,
                ..ReadOptions::minimal()
            };
            for file in files {
                match render_rewritten_file(cwd, &file, options) {
                    Ok(rendered) => stdout.push_str(&rendered),
                    Err(err) => {
                        status = 1;
                        stderr.push_str(&format!("cat: {file}: {}\n", err.root_cause()));
                    }
                }
            }
        }
        ReadRewrite::Head { file, max_lines } => match render_rewritten_file(
            cwd,
            &file,
            ReadOptions {
                max_lines,
                ..ReadOptions::minimal()
            },
        ) {
            Ok(rendered) => stdout.push_str(&rendered),
            Err(err) => {
                status = 1;
                stderr.push_str(&format!("head: {file}: {}\n", err.root_cause()));
            }
        },
        ReadRewrite::Tail { file, tail_lines } => match render_rewritten_file(
            cwd,
            &file,
            ReadOptions {
                tail_lines: Some(tail_lines),
                ..ReadOptions::minimal()
            },
        ) {
            Ok(rendered) => stdout.push_str(&rendered),
            Err(err) => {
                status = 1;
                stderr.push_str(&format!("tail: {file}: {}\n", err.root_cause()));
            }
        },
    }

    if status == 0 && stderr.is_empty() {
        return Ok(stdout);
    }

    if !stderr.is_empty() {
        if !stdout.is_empty() && !stdout.ends_with('\n') {
            stdout.push('\n');
        }
        stdout.push_str(&stderr);
    }
    if status != 0 {
        stdout.push_str(&format!("[exit status: {status}]\n"));
    }
    Ok(stdout)
}

fn render_rewritten_file(cwd: &Path, file: &str, options: ReadOptions) -> Result<String> {
    let content = fs::read_file(cwd, &[], file)?;
    Ok(read_filter::render(Path::new(file), &content, options))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_registry::unchecked_permission_context;
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
        assert!(result.content.contains("hello"));
        assert!(result.content.contains("status:"));
        assert!(result.truncation.is_none());
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
        assert!(result.content.contains("stderr:\noops"));
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
        assert!(result.content.contains("status: exit status: 42"));
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
        assert!(result.content.contains("status: exit status: 0"));
    }

    #[tokio::test]
    async fn bash_cat_reads_file_internally() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "alpha\nbravo\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "cat note.txt")
            .await
            .unwrap();

        assert_eq!(result.content, "alpha\nbravo");
    }

    #[tokio::test]
    async fn bash_cat_line_numbers_maps_to_internal_read() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "alpha\nbravo\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "cat -n note.txt")
            .await
            .unwrap();

        assert_eq!(result.content, "1 │ alpha\n2 │ bravo\n");
    }

    #[tokio::test]
    async fn bash_cat_applies_rtk_minimal_filter() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(
            cwd.join("main.rs"),
            "// hidden comment\n\n\nfn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "cat main.rs")
            .await
            .unwrap();

        assert_eq!(result.content, "fn main() {\n    println!(\"hi\");\n}");
    }

    #[tokio::test]
    async fn bash_cat_with_glob_stays_native_shell() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(cwd.join("b.txt"), "bravo\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "cat *.txt")
            .await
            .unwrap();

        assert!(result.content.contains("alpha\n"));
        assert!(result.content.contains("bravo\n"));
        assert!(!result.content.contains("failed to canonicalize"));
    }

    #[tokio::test]
    async fn bash_head_numeric_flag_maps_to_internal_read_window() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "one\ntwo\nthree\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "head -2 note.txt")
            .await
            .unwrap();

        assert_eq!(result.content, "one\n[2 more lines]");
        assert!(!result.content.contains("three"));
    }

    #[tokio::test]
    async fn bash_head_lines_eq_maps_to_internal_read_window() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "one\ntwo\nthree\n").unwrap();

        let result = bash(
            &unchecked_permission_context(),
            &cwd,
            5,
            "head --lines=1 note.txt",
        )
        .await
        .unwrap();

        assert_eq!(result.content, "[3 more lines]");
        assert!(!result.content.contains("two"));
    }

    #[tokio::test]
    async fn bash_head_zero_lines_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "one\ntwo\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "head -0 note.txt")
            .await
            .unwrap();

        assert_eq!(result.content, "");
    }

    #[tokio::test]
    async fn bash_head_plain_file_maps_to_internal_read_like_rtk() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "one\ntwo\nthree\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "head note.txt")
            .await
            .unwrap();

        assert_eq!(result.content, "one\ntwo\nthree");
    }

    #[tokio::test]
    async fn bash_head_multi_file_stays_native_shell() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(cwd.join("b.txt"), "bravo\n").unwrap();

        let result = bash(
            &unchecked_permission_context(),
            &cwd,
            5,
            "head -1 a.txt b.txt",
        )
        .await
        .unwrap();

        assert!(result.content.contains("==> a.txt <=="));
        assert!(result.content.contains("==> b.txt <=="));
    }

    #[tokio::test]
    async fn bash_tail_flags_map_to_internal_read_window() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "one\ntwo\nthree\n").unwrap();

        let numeric = bash(&unchecked_permission_context(), &cwd, 5, "tail -2 note.txt")
            .await
            .unwrap();
        let split = bash(
            &unchecked_permission_context(),
            &cwd,
            5,
            "tail -n 2 note.txt",
        )
        .await
        .unwrap();
        let long_eq = bash(
            &unchecked_permission_context(),
            &cwd,
            5,
            "tail --lines=2 note.txt",
        )
        .await
        .unwrap();
        let long_space = bash(
            &unchecked_permission_context(),
            &cwd,
            5,
            "tail --lines 2 note.txt",
        )
        .await
        .unwrap();

        for result in [numeric, split, long_eq, long_space] {
            assert_eq!(result.content, "two\nthree");
            assert!(!result.content.contains("one"));
        }
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
    async fn bash_spills_large_output_and_appends_trailer() {
        let result = bash(
            &unchecked_permission_context(),
            Path::new("/tmp"),
            30,
            "seq 1 200000",
        )
        .await
        .unwrap();
        let spill_path = match result.truncation.as_ref().expect("spill truncation") {
            crate::tool_registry::output_accumulator::AccumulatorTruncation::Spilled {
                path,
                ..
            } => path.clone(),
            other => panic!("expected Spilled, got {other:?}"),
        };
        assert!(
            spill_path.is_absolute(),
            "spill path should be absolute: {}",
            spill_path.display()
        );
        let trailer_marker = "[Full output: ";
        let trailer_at = result
            .content
            .rfind(trailer_marker)
            .expect("trailer present in bash output");
        let trailer_tail = &result.content[trailer_at + trailer_marker.len()..];
        let path_end = trailer_tail.find(']').expect("closing bracket on trailer");
        let path_str = &trailer_tail[..path_end];
        assert_eq!(Path::new(path_str), spill_path);
        let on_disk = std::fs::read_to_string(&spill_path).expect("spill file readable");
        assert!(
            on_disk.contains("\n200000\n") || on_disk.ends_with("200000\n"),
            "spill file is missing line 200000 (last 80 chars: {:?})",
            &on_disk[on_disk.len().saturating_sub(80)..]
        );
        // The real nav data dir isn't hermetic here; clean up this run's
        // spill so successive `cargo test` invocations don't accumulate
        // files until the 7-day sweep catches them.
        let _ = std::fs::remove_file(&spill_path);
    }

    #[tokio::test]
    async fn bash_cat_rewrite_bounds_large_file_without_spill() {
        // The rewrite shortcut for `cat` goes through the same accumulator
        // path as the native sandbox path, so a large rewritten read must
        // produce the head/tail-bounded marker the global cap emits — and
        // it must NOT trip the spill trailer (rewrites never spill: the
        // rtk filter's byte/line limits keep the rendered output far below
        // MAX_ROLLING_BYTES).
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let line = "z".repeat(200) + "\n";
        let payload = line.repeat(400); // ~80 KB > MAX_BYTES, < MAX_ROLLING_BYTES
        std::fs::write(cwd.join("big.txt"), &payload).unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "cat big.txt")
            .await
            .unwrap();
        assert!(
            result.content.contains("[truncated"),
            "rewrite-path large output should be bounded: {}",
            &result.content[..result.content.len().min(120)]
        );
        assert_eq!(
            result.truncation,
            Some(crate::tool_registry::output_accumulator::AccumulatorTruncation::Bound),
            "rewrite path should bound in-memory, not spill"
        );
        assert!(
            !result.content.contains("[Full output:"),
            "rewrite path should not spill"
        );
        assert!(
            result.content.len() < 80 * 1024,
            "bounded rewrite output was {} bytes",
            result.content.len()
        );
    }

    #[tokio::test]
    async fn bash_runs_in_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let result = bash(&unchecked_permission_context(), &cwd, 5, "pwd")
            .await
            .unwrap();
        assert!(
            result
                .content
                .contains(&format!("stdout:\n{}", cwd.display()))
        );
    }
}
