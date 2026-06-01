//! Linked-git-worktree guardrails shared by path tools and bash.
//!
//! A regular checkout stays flexible. In a linked worktree, paths that point at
//! the main checkout are treated as references to the active worktree, while
//! sibling worktrees are blocked so one run cannot accidentally edit another.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub(super) struct LinkedWorktree {
    current_root: PathBuf,
    main_root: PathBuf,
    sibling_roots: Vec<PathBuf>,
}

pub(super) enum PathRewrite {
    Path(PathBuf),
    Block(String),
}

pub(super) fn rewrite_absolute_path(cwd: &Path, requested: &Path) -> PathRewrite {
    let requested = normalize_existing_prefix(requested);
    let Some(worktree) = LinkedWorktree::for_cwd(cwd) else {
        return PathRewrite::Path(requested);
    };

    if requested.starts_with(&worktree.current_root) {
        return PathRewrite::Path(requested);
    }

    if requested.starts_with(&worktree.main_root) {
        let relative = requested
            .strip_prefix(&worktree.main_root)
            .unwrap_or_else(|_| Path::new(""));
        return PathRewrite::Path(worktree.current_root.join(relative));
    }

    if worktree
        .sibling_roots
        .iter()
        .any(|root| requested.starts_with(root))
    {
        return PathRewrite::Block(format!(
            "path targets another git worktree: {}",
            requested.display()
        ));
    }

    PathRewrite::Path(requested)
}

pub(in crate::tools) fn rewrite_bash_command(cwd: &Path, command: &str) -> Result<String, String> {
    let Some(worktree) = LinkedWorktree::for_cwd(cwd) else {
        return Ok(command.to_owned());
    };

    let rewritten = replace_path_prefix(
        command,
        &display_path(&worktree.main_root),
        &display_path(&worktree.current_root),
    );

    for sibling in &worktree.sibling_roots {
        let sibling = display_path(sibling);
        if contains_path_prefix(&rewritten, &sibling) {
            return Err(format!(
                "blocked bash command: path targets another git worktree ({sibling})"
            ));
        }
    }

    if has_parent_traversal(&rewritten) {
        return Err(
            "blocked bash command: parent-directory traversal is not allowed in a git worktree"
                .to_owned(),
        );
    }

    Ok(rewritten)
}

impl LinkedWorktree {
    fn for_cwd(cwd: &Path) -> Option<Self> {
        let rev_parse = git_lines(
            cwd,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ],
        )?;
        if rev_parse.len() < 3 {
            return None;
        }

        let current_root = normalize(Path::new(&rev_parse[0]));
        let git_dir = normalize(Path::new(&rev_parse[1]));
        let common_dir = normalize(Path::new(&rev_parse[2]));
        if git_dir == common_dir {
            return None;
        }

        let roots = worktree_roots(cwd)?;
        let main_root = roots.first()?.clone();
        if !roots.contains(&current_root) || main_root == current_root {
            return None;
        }

        let sibling_roots = roots
            .into_iter()
            .filter(|root| *root != current_root && *root != main_root)
            .collect();

        Some(Self {
            current_root,
            main_root,
            sibling_roots,
        })
    }
}

fn worktree_roots(cwd: &Path) -> Option<Vec<PathBuf>> {
    let roots: Vec<PathBuf> = git_lines(cwd, &["worktree", "list", "--porcelain"])?
        .into_iter()
        .filter_map(|line| {
            line.strip_prefix("worktree ")
                .map(|path| normalize(Path::new(path)))
        })
        .collect();
    (!roots.is_empty()).then_some(roots)
}

fn git_lines(cwd: &Path, args: &[&str]) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    })
}

fn replace_path_prefix(command: &str, old: &str, new: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    for (index, _) in command.match_indices(old) {
        if index < last {
            continue;
        }
        let end = index + old.len();
        if !path_boundary(command, end) {
            continue;
        }
        out.push_str(&command[last..index]);
        out.push_str(new);
        last = end;
    }
    out.push_str(&command[last..]);
    out
}

fn contains_path_prefix(command: &str, path: &str) -> bool {
    command
        .match_indices(path)
        .any(|(index, _)| path_boundary(command, index + path.len()))
}

fn path_boundary(command: &str, index: usize) -> bool {
    command[index..]
        .chars()
        .next()
        .map(|ch| ch == '/')
        .unwrap_or(true)
}

fn has_parent_traversal(command: &str) -> bool {
    let bytes = command.as_bytes();
    for index in 0..bytes.len().saturating_sub(1) {
        if bytes[index] != b'.' || bytes[index + 1] != b'.' {
            continue;
        }
        let previous_ok = index == 0 || is_path_separator(bytes[index - 1]);
        let next = bytes.get(index + 2).copied();
        let next_ok = next.is_none_or(is_parent_traversal_suffix);
        if previous_ok && next_ok {
            return true;
        }
    }
    false
}

fn is_path_separator(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | b'\''
            | b'"'
            | b'`'
            | b';'
            | b'|'
            | b'&'
            | b'<'
            | b'>'
            | b'/'
    )
}

fn is_parent_traversal_suffix(byte: u8) -> bool {
    matches!(byte, b'/' | b'\'' | b'"' | b'`')
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn normalize_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return normalize(&canonical);
    }

    let mut missing = Vec::new();
    let mut current = path;
    loop {
        if let Ok(canonical) = std::fs::canonicalize(current) {
            let mut out = normalize(&canonical);
            for component in missing.iter().rev() {
                out.push(component);
            }
            return normalize(&out);
        }
        if let Some(name) = current.file_name() {
            missing.push(name.to_owned());
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent;
    }

    normalize(path)
}
