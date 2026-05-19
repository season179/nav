use anyhow::Result;
use std::path::{Component, Path};
use std::time::Duration;

use crate::sandbox::SandboxRequest;
use crate::tools::preflight::PermissionContext;
use crate::tools::read_filter::{self, ReadOptions};
use crate::{permissions::bash_parse::parse_command_pipeline, tools::fs};

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
) -> Result<String> {
    if let Some(rewrite) = read_rewrite(command) {
        return run_read_rewrite(cwd, rewrite);
    }

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
    async fn bash_cat_reads_file_internally() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "alpha\nbravo\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "cat note.txt")
            .await
            .unwrap();

        assert_eq!(result, "alpha\nbravo");
    }

    #[tokio::test]
    async fn bash_cat_line_numbers_maps_to_internal_read() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "alpha\nbravo\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "cat -n note.txt")
            .await
            .unwrap();

        assert_eq!(result, "1 │ alpha\n2 │ bravo\n");
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

        assert_eq!(result, "fn main() {\n    println!(\"hi\");\n}");
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

        assert!(result.contains("alpha\n"));
        assert!(result.contains("bravo\n"));
        assert!(!result.contains("failed to canonicalize"));
    }

    #[tokio::test]
    async fn bash_head_numeric_flag_maps_to_internal_read_window() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "one\ntwo\nthree\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "head -2 note.txt")
            .await
            .unwrap();

        assert_eq!(result, "one\n[2 more lines]");
        assert!(!result.contains("three"));
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

        assert_eq!(result, "[3 more lines]");
        assert!(!result.contains("two"));
    }

    #[tokio::test]
    async fn bash_head_zero_lines_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "one\ntwo\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "head -0 note.txt")
            .await
            .unwrap();

        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn bash_head_plain_file_maps_to_internal_read_like_rtk() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("note.txt"), "one\ntwo\nthree\n").unwrap();

        let result = bash(&unchecked_permission_context(), &cwd, 5, "head note.txt")
            .await
            .unwrap();

        assert_eq!(result, "one\ntwo\nthree");
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

        assert!(result.contains("==> a.txt <=="));
        assert!(result.contains("==> b.txt <=="));
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
            assert_eq!(result, "two\nthree");
            assert!(!result.contains("one"));
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
    async fn bash_runs_in_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let result = bash(&unchecked_permission_context(), &cwd, 5, "pwd")
            .await
            .unwrap();
        assert!(result.contains(&format!("stdout:\n{}", cwd.display())));
    }
}
