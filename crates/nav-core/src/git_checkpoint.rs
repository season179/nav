use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const NAV_CHECKPOINT_MARKER: &str = "nav checkpoint";
const NAV_STASH_MARKER: &str = "nav stash";
const NAV_RESTORE_SAFETY_MARKER: &str = "nav restore safety";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCheckpointAction {
    Checkpoint,
    Stash,
    Restore,
}

impl GitCheckpointAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::Stash => "stash",
            Self::Restore => "restore",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCheckpointStatus {
    Created,
    Restored,
    Failed,
    NoChanges,
}

impl GitCheckpointStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Restored => "restored",
            Self::Failed => "failed",
            Self::NoChanges => "no_changes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCheckpointOutcome {
    pub action: GitCheckpointAction,
    pub status: GitCheckpointStatus,
    pub stash_ref: Option<String>,
    pub stash_oid: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStashEntry {
    pub stash_ref: String,
    pub oid: String,
    pub subject: String,
}

/// Returns true when `cwd` is inside a Git work tree. A missing `git` binary
/// is treated as `false` so optional automatic checkpoints can skip quietly.
pub fn is_git_repo(cwd: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Create a reversible checkpoint while preserving the current worktree. This
/// is implemented as `stash push --include-untracked` followed by
/// `stash apply --index <oid>` so the stash object remains as a restore point
/// but the user keeps working with the same visible files.
pub fn checkpoint(
    cwd: &Path,
    session_id: Option<&str>,
    label: Option<&str>,
) -> Result<GitCheckpointOutcome> {
    ensure_git_repo(cwd)?;
    if !has_worktree_changes(cwd)? {
        return Ok(GitCheckpointOutcome {
            action: GitCheckpointAction::Checkpoint,
            status: GitCheckpointStatus::NoChanges,
            stash_ref: None,
            stash_oid: None,
            message: "no local changes to checkpoint".to_string(),
        });
    }

    let message = nav_message(NAV_CHECKPOINT_MARKER, session_id, label);
    let entry = stash_push(cwd, &message)?;
    git_output(cwd, &["stash", "apply", "--index", &entry.oid])
        .with_context(|| format!("created {} but failed to re-apply it", entry.oid))?;

    Ok(GitCheckpointOutcome {
        action: GitCheckpointAction::Checkpoint,
        status: GitCheckpointStatus::Created,
        stash_ref: Some(entry.stash_ref),
        stash_oid: Some(entry.oid),
        message,
    })
}

/// Save local changes into a nav-labelled stash and leave the worktree clean.
pub fn stash(
    cwd: &Path,
    session_id: Option<&str>,
    label: Option<&str>,
) -> Result<GitCheckpointOutcome> {
    ensure_git_repo(cwd)?;
    if !has_worktree_changes(cwd)? {
        return Ok(GitCheckpointOutcome {
            action: GitCheckpointAction::Stash,
            status: GitCheckpointStatus::NoChanges,
            stash_ref: None,
            stash_oid: None,
            message: "no local changes to stash".to_string(),
        });
    }

    let message = nav_message(NAV_STASH_MARKER, session_id, label);
    let entry = stash_push(cwd, &message)?;
    Ok(GitCheckpointOutcome {
        action: GitCheckpointAction::Stash,
        status: GitCheckpointStatus::Created,
        stash_ref: Some(entry.stash_ref),
        stash_oid: Some(entry.oid),
        message,
    })
}

/// Restore a checkpoint or stash by ref/OID. If `target` is omitted, the
/// newest nav checkpoint/stash is used. Existing local changes are first saved
/// to a safety stash, so restore is reversible even when the current tree is
/// dirty.
pub fn restore(cwd: &Path, target: Option<&str>) -> Result<GitCheckpointOutcome> {
    ensure_git_repo(cwd)?;
    let target_entry = resolve_restore_target(cwd, target)?;
    let safety = if has_worktree_changes(cwd)? {
        Some(stash_push(
            cwd,
            &nav_message(
                NAV_RESTORE_SAFETY_MARKER,
                None,
                Some(target_entry.stash_ref.as_str()),
            ),
        )?)
    } else {
        None
    };

    if let Err(err) = git_output(cwd, &["stash", "apply", "--index", &target_entry.oid]) {
        if let Some(safety) = safety {
            bail!(
                "failed to restore {} ({err:#}); current changes were preserved in {} ({})",
                target_entry.stash_ref,
                safety.stash_ref,
                safety.oid
            );
        }
        return Err(err).with_context(|| format!("failed to restore {}", target_entry.stash_ref));
    }

    Ok(GitCheckpointOutcome {
        action: GitCheckpointAction::Restore,
        status: GitCheckpointStatus::Restored,
        stash_ref: Some(target_entry.stash_ref),
        stash_oid: Some(target_entry.oid),
        message: safety
            .map(|entry| {
                format!(
                    "restored checkpoint; prior changes saved as {}",
                    entry.stash_ref
                )
            })
            .unwrap_or_else(|| "restored checkpoint".to_string()),
    })
}

pub fn list_nav_stashes(cwd: &Path) -> Result<Vec<GitStashEntry>> {
    ensure_git_repo(cwd)?;
    let raw = git_output(cwd, &["stash", "list", "--format=%gd%x00%H%x00%s"])?;
    Ok(parse_stash_list(&raw)
        .into_iter()
        .filter(|entry| is_nav_stash_subject(&entry.subject))
        .collect())
}

fn resolve_restore_target(cwd: &Path, target: Option<&str>) -> Result<GitStashEntry> {
    if let Some(target) = target.map(str::trim).filter(|t| !t.is_empty()) {
        let oid = git_output(cwd, &["rev-parse", "--verify", target])?
            .trim()
            .to_string();
        return Ok(GitStashEntry {
            stash_ref: target.to_string(),
            oid,
            subject: target.to_string(),
        });
    }
    list_nav_stashes(cwd)?
        .into_iter()
        .find(|entry| is_default_restore_candidate(&entry.subject))
        .ok_or_else(|| anyhow::anyhow!("no nav checkpoint or stash found"))
}

fn ensure_git_repo(cwd: &Path) -> Result<()> {
    if is_git_repo(cwd) {
        Ok(())
    } else {
        bail!("not a git work tree: {}", cwd.display())
    }
}

fn has_worktree_changes(cwd: &Path) -> Result<bool> {
    let out = git_output(cwd, &["status", "--porcelain=v1", "-z"])?;
    Ok(!out.is_empty())
}

fn stash_push(cwd: &Path, message: &str) -> Result<GitStashEntry> {
    git_output(
        cwd,
        &["stash", "push", "--include-untracked", "-m", message],
    )?;
    let oid = git_output(cwd, &["rev-parse", "--verify", "stash@{0}"])?
        .trim()
        .to_string();
    Ok(GitStashEntry {
        stash_ref: "stash@{0}".to_string(),
        oid,
        subject: message.to_string(),
    })
}

fn nav_message(kind: &str, session_id: Option<&str>, label: Option<&str>) -> String {
    let mut message = kind.to_string();
    if let Some(session) = session_id.map(short_id).filter(|s| !s.is_empty()) {
        message.push(' ');
        message.push_str(&session);
    }
    if let Some(label) = label.map(clean_label).filter(|s| !s.is_empty()) {
        message.push_str(": ");
        message.push_str(&label);
    } else {
        message.push_str(&format!(" @{}", now_secs()));
    }
    message
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn clean_label(label: &str) -> String {
    label.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_stash_list(raw: &str) -> Vec<GitStashEntry> {
    raw.lines()
        .filter_map(|line| {
            let mut parts = line.split('\0');
            let stash_ref = parts.next()?.to_string();
            let oid = parts.next()?.to_string();
            let subject = parts.next().unwrap_or_default().to_string();
            (!stash_ref.is_empty() && !oid.is_empty()).then_some(GitStashEntry {
                stash_ref,
                oid,
                subject,
            })
        })
        .collect()
}

fn is_nav_stash_subject(subject: &str) -> bool {
    is_default_restore_candidate(subject)
        || starts_with_nav_marker(subject, NAV_RESTORE_SAFETY_MARKER)
}

fn is_default_restore_candidate(subject: &str) -> bool {
    starts_with_nav_marker(subject, NAV_CHECKPOINT_MARKER)
        || starts_with_nav_marker(subject, NAV_STASH_MARKER)
}

fn starts_with_nav_marker(subject: &str, marker: &str) -> bool {
    let body = subject
        .split_once(": ")
        .map(|(_, body)| body)
        .unwrap_or(subject);
    body == marker
        || body
            .strip_prefix(marker)
            .is_some_and(|suffix| suffix.starts_with(' ') || suffix.starts_with(": "))
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    output_to_string(output, args)
}

fn output_to_string(output: Output, args: &[&str]) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {stderr}", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed with {status}");
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        git(dir.path(), &["init"]);
        fs::write(dir.path().join("tracked.txt"), "base\n").unwrap();
        git(dir.path(), &["add", "tracked.txt"]);
        git(
            dir.path(),
            &[
                "-c",
                "user.name=Nav Test",
                "-c",
                "user.email=nav@example.test",
                "commit",
                "-m",
                "init",
            ],
        );
        dir
    }

    #[test]
    fn checkpoint_preserves_worktree_and_creates_nav_stash() {
        let dir = repo();
        fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
        fs::write(dir.path().join("untracked.txt"), "new\n").unwrap();

        let outcome = checkpoint(dir.path(), Some("01ABCDEF9999"), Some("before risky turn"))
            .expect("checkpoint");

        assert_eq!(outcome.status, GitCheckpointStatus::Created);
        assert!(outcome.message.contains("nav checkpoint 01ABCDEF"));
        assert_eq!(
            fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "dirty\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("untracked.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(list_nav_stashes(dir.path()).unwrap().len(), 1);
    }

    #[test]
    fn checkpoint_preserves_index_state() {
        let dir = repo();
        fs::write(dir.path().join("tracked.txt"), "staged\n").unwrap();
        git(dir.path(), &["add", "tracked.txt"]);
        fs::write(dir.path().join("tracked.txt"), "unstaged\n").unwrap();

        checkpoint(dir.path(), None, Some("index state")).expect("checkpoint");

        let status = git_output(dir.path(), &["status", "--porcelain=v1"]).unwrap();
        assert!(status.contains("MM tracked.txt"), "{status}");
    }

    #[test]
    fn stash_cleans_worktree() {
        let dir = repo();
        fs::write(dir.path().join("tracked.txt"), "dirty\n").unwrap();
        fs::write(dir.path().join("untracked.txt"), "new\n").unwrap();

        let outcome = stash(dir.path(), None, Some("hold changes")).expect("stash");

        assert_eq!(outcome.status, GitCheckpointStatus::Created);
        assert_eq!(
            fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "base\n"
        );
        assert!(!dir.path().join("untracked.txt").exists());
        assert_eq!(list_nav_stashes(dir.path()).unwrap().len(), 1);
    }

    #[test]
    fn restore_safety_stashes_current_changes_before_apply() {
        let dir = repo();
        fs::write(dir.path().join("tracked.txt"), "checkpoint\n").unwrap();
        fs::write(dir.path().join("checkpoint-only.txt"), "kept\n").unwrap();
        let checkpoint = checkpoint(dir.path(), None, Some("target")).expect("checkpoint");

        fs::write(dir.path().join("tracked.txt"), "current\n").unwrap();
        fs::write(dir.path().join("current-only.txt"), "safety\n").unwrap();

        let restored = restore(dir.path(), checkpoint.stash_oid.as_deref()).expect("restore");

        assert_eq!(restored.status, GitCheckpointStatus::Restored);
        assert_eq!(
            fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "checkpoint\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("checkpoint-only.txt")).unwrap(),
            "kept\n"
        );
        assert!(!dir.path().join("current-only.txt").exists());
        let nav_stashes = list_nav_stashes(dir.path()).unwrap();
        assert_eq!(nav_stashes.len(), 2);
        assert!(nav_stashes[0].subject.contains(NAV_RESTORE_SAFETY_MARKER));
        assert!(nav_stashes[1].subject.contains(NAV_CHECKPOINT_MARKER));
    }

    #[test]
    fn default_restore_skips_restore_safety_stashes() {
        let dir = repo();
        fs::write(dir.path().join("tracked.txt"), "checkpoint\n").unwrap();
        checkpoint(dir.path(), None, Some("target")).expect("checkpoint");

        fs::write(dir.path().join("tracked.txt"), "current\n").unwrap();
        restore(dir.path(), None).expect("restore target");

        let restored_again = restore(dir.path(), None).expect("restore defaults to target");

        assert_eq!(restored_again.status, GitCheckpointStatus::Restored);
        assert_eq!(
            fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
            "checkpoint\n"
        );
        assert!(
            restored_again
                .message
                .contains("prior changes saved as stash@{0}")
        );
    }

    #[test]
    fn nav_stash_subject_matching_requires_marker_prefix() {
        assert!(is_nav_stash_subject("On main: nav checkpoint: target"));
        assert!(is_nav_stash_subject("nav stash @123"));
        assert!(is_nav_stash_subject(
            "On main: nav restore safety: stash@{1}"
        ));
        assert!(!is_nav_stash_subject("On main: not a nav checkpoint"));
        assert!(!is_nav_stash_subject("manual nav stash note"));
    }

    #[test]
    fn no_changes_returns_no_changes_without_creating_stash() {
        let dir = repo();

        let outcome = checkpoint(dir.path(), None, None).expect("checkpoint");

        assert_eq!(outcome.status, GitCheckpointStatus::NoChanges);
        assert!(list_nav_stashes(dir.path()).unwrap().is_empty());
    }
}
