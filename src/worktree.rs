//! Session-time git worktree creation.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use uuid::Uuid;

pub(crate) struct CreatedWorktree {
    pub path: PathBuf,
    pub branch: String,
}

pub(crate) fn create_session_worktree(cwd: &Path) -> Result<CreatedWorktree, String> {
    let repo_root = git_stdout(cwd, &["rev-parse", "--show-toplevel"])?;
    let repo_root = PathBuf::from(repo_root.trim());
    let container_root = main_worktree_root(&repo_root).unwrap_or(repo_root);
    ensure_local_nav_ignored(&container_root);

    let base = base_ref(&container_root)?;
    let id = Uuid::now_v7().to_string();
    let branch = format!("nav-wt/{id}");
    let path = container_root
        .join(".nav")
        .join("worktrees")
        .join(format!("nav-wt-{id}"));
    let path_arg = path.to_string_lossy().into_owned();

    git_stdout(
        &container_root,
        &["worktree", "add", "-b", &branch, &path_arg, &base],
    )?;

    Ok(CreatedWorktree { path, branch })
}

fn main_worktree_root(cwd: &Path) -> Option<PathBuf> {
    git_stdout(cwd, &["worktree", "list", "--porcelain"])
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("worktree ").map(PathBuf::from))
}

fn base_ref(repo_root: &Path) -> Result<String, String> {
    for branch in ["main", "master"] {
        if git_status(
            repo_root,
            &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        ) {
            return Ok(branch.to_owned());
        }
    }

    let head = git_stdout(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let head = head.trim();
    if head.is_empty() {
        return Err("could not resolve a base ref for worktree session".to_owned());
    }
    Ok(head.to_owned())
}

fn ensure_local_nav_ignored(repo_root: &Path) {
    let Ok(exclude) = git_stdout(repo_root, &["rev-parse", "--git-path", "info/exclude"]) else {
        return;
    };
    let exclude = exclude.trim();
    if exclude.is_empty() {
        return;
    }
    let exclude_path = if Path::new(exclude).is_absolute() {
        PathBuf::from(exclude)
    } else {
        repo_root.join(exclude)
    };

    let Ok(content) = fs::read_to_string(&exclude_path).or_else(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(String::new())
        } else {
            Err(error)
        }
    }) else {
        return;
    };
    if content
        .lines()
        .any(|line| matches!(line.trim(), ".nav" | ".nav/"))
    {
        return;
    }

    if let Some(parent) = exclude_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let separator = if content.is_empty() || content.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let _ = fs::write(exclude_path, format!("{content}{separator}.nav/\n"));
}

fn git_status(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(stderr.trim().to_owned())
    }
}
