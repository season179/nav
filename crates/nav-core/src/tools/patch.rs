use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::ToolResult;
use super::fs::{relative_path, resolve_create_path, resolve_delete_path, resolve_inside};
use crate::mutation::{FileChangeKind, FileChangeSummary, MutationResult, summarize_changes};

#[derive(Debug)]
enum PatchOperation {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_path: Option<String>,
        hunks: Vec<Hunk>,
    },
}

#[derive(Debug, Default)]
struct Hunk {
    lines: Vec<HunkLine>,
}

#[derive(Debug)]
enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

struct PlannedChange {
    source_path: Option<PathBuf>,
    target_path: Option<PathBuf>,
    source_label: String,
    target_label: String,
    before: String,
    after: String,
    summary_path: String,
    kind: FileChangeKind,
}

pub(super) fn apply_patch(cwd: &Path, patch: &str) -> Result<ToolResult> {
    let operations = parse_patch(patch)?;
    let planned = plan_changes(cwd, operations)?;
    apply_changes(&planned)?;

    let changes = planned
        .iter()
        .map(|change| {
            FileChangeSummary::new(
                change.summary_path.clone(),
                change.kind.clone(),
                &change.before,
                &change.after,
                &change.source_label,
                &change.target_label,
            )
        })
        .collect::<Vec<_>>();
    let summary = summarize_changes(&changes);
    Ok(ToolResult::mutation(
        summary.clone(),
        MutationResult { summary, changes },
    ))
}

pub(super) fn target_paths_from_patch(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|line| {
            line.strip_prefix("*** Update File: ")
                .or_else(|| line.strip_prefix("*** Add File: "))
                .or_else(|| line.strip_prefix("*** Delete File: "))
                .or_else(|| line.strip_prefix("*** Move to: "))
        })
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_patch(patch: &str) -> Result<Vec<PatchOperation>> {
    let lines = patch.lines().collect::<Vec<_>>();
    if lines.len() < 2
        || lines.first() != Some(&"*** Begin Patch")
        || lines.last() != Some(&"*** End Patch")
    {
        bail!("patch must start with `*** Begin Patch` and end with `*** End Patch`");
    }

    let mut operations = Vec::new();
    let mut idx = 1;
    while idx < lines.len() - 1 {
        let line = lines[idx];
        if line.trim().is_empty() {
            idx += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let path = parse_relative_patch_path(path)?;
            idx += 1;
            let mut content = Vec::new();
            while idx < lines.len() - 1 && !lines[idx].starts_with("*** ") {
                let content_line = lines[idx]
                    .strip_prefix('+')
                    .ok_or_else(|| anyhow!("add file lines must start with `+`"))?;
                content.push(content_line.to_string());
                idx += 1;
            }
            operations.push(PatchOperation::Add {
                path,
                lines: content,
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            let path = parse_relative_patch_path(path)?;
            operations.push(PatchOperation::Delete { path });
            idx += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            let path = parse_relative_patch_path(path)?;
            idx += 1;
            let mut move_path = None;
            if idx < lines.len() - 1
                && let Some(next_path) = lines[idx].strip_prefix("*** Move to: ")
            {
                move_path = Some(parse_relative_patch_path(next_path)?);
                idx += 1;
            }
            let mut hunks = Vec::new();
            let mut current = Hunk::default();
            while idx < lines.len() - 1
                && (!lines[idx].starts_with("*** ") || lines[idx] == "*** End of File")
            {
                let hunk_line = lines[idx];
                if hunk_line.starts_with("@@") {
                    if !current.lines.is_empty() {
                        hunks.push(current);
                        current = Hunk::default();
                    }
                    idx += 1;
                    continue;
                }
                if hunk_line == "*** End of File" {
                    idx += 1;
                    continue;
                }
                let parsed = if let Some(text) = hunk_line.strip_prefix(' ') {
                    HunkLine::Context(text.to_string())
                } else if let Some(text) = hunk_line.strip_prefix('-') {
                    HunkLine::Remove(text.to_string())
                } else if let Some(text) = hunk_line.strip_prefix('+') {
                    HunkLine::Add(text.to_string())
                } else {
                    bail!("update hunk lines must start with space, `-`, `+`, or `@@`");
                };
                current.lines.push(parsed);
                idx += 1;
            }
            if !current.lines.is_empty() {
                hunks.push(current);
            }
            operations.push(PatchOperation::Update {
                path,
                move_path,
                hunks,
            });
            continue;
        }

        bail!("unknown patch header: {line}");
    }

    if operations.is_empty() {
        bail!("patch does not contain any file changes");
    }
    Ok(operations)
}

fn parse_relative_patch_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    relative_path(trimmed)?;
    Ok(trimmed.to_string())
}

fn plan_changes(cwd: &Path, operations: Vec<PatchOperation>) -> Result<Vec<PlannedChange>> {
    let mut seen_sources = BTreeSet::new();
    let mut seen_targets = BTreeSet::new();
    let mut planned = Vec::new();

    for operation in operations {
        match operation {
            PatchOperation::Add { path, lines } => {
                reject_duplicate(&mut seen_targets, &path)?;
                let target_path = resolve_create_path(cwd, &path)?;
                if target_path.exists() {
                    bail!("{} already exists", target_path.display());
                }
                let after = join_lines(&lines, !lines.is_empty());
                planned.push(PlannedChange {
                    source_path: None,
                    target_path: Some(target_path),
                    source_label: "/dev/null".to_string(),
                    target_label: format!("b/{path}"),
                    before: String::new(),
                    after,
                    summary_path: path,
                    kind: FileChangeKind::Add,
                });
            }
            PatchOperation::Delete { path } => {
                reject_duplicate(&mut seen_sources, &path)?;
                let source_path = resolve_delete_path(cwd, &path)?;
                let before = fs::read_to_string(&source_path)
                    .with_context(|| format!("failed to read {}", source_path.display()))?;
                planned.push(PlannedChange {
                    source_path: Some(source_path),
                    target_path: None,
                    source_label: format!("a/{path}"),
                    target_label: "/dev/null".to_string(),
                    before,
                    after: String::new(),
                    summary_path: path,
                    kind: FileChangeKind::Delete,
                });
            }
            PatchOperation::Update {
                path,
                move_path,
                hunks,
            } => {
                reject_duplicate(&mut seen_sources, &path)?;
                let source_path = resolve_inside(cwd, &path)?;
                let before = fs::read_to_string(&source_path)
                    .with_context(|| format!("failed to read {}", source_path.display()))?;
                let after = apply_hunks(&before, &hunks)
                    .with_context(|| format!("failed to apply patch to {path}"))?;
                let (target_path, target_label) = if let Some(move_path) = &move_path {
                    reject_duplicate(&mut seen_targets, move_path)?;
                    let target_path = resolve_create_path(cwd, move_path)?;
                    if target_path.exists() {
                        bail!("{} already exists", target_path.display());
                    }
                    (target_path, format!("b/{move_path}"))
                } else {
                    reject_duplicate(&mut seen_targets, &path)?;
                    (source_path.clone(), format!("b/{path}"))
                };
                planned.push(PlannedChange {
                    source_path: Some(source_path),
                    target_path: Some(target_path),
                    source_label: format!("a/{path}"),
                    target_label,
                    before,
                    after,
                    summary_path: path,
                    kind: FileChangeKind::Update { move_path },
                });
            }
        }
    }

    Ok(planned)
}

fn reject_duplicate(seen: &mut BTreeSet<String>, path: &str) -> Result<()> {
    if !seen.insert(path.to_string()) {
        bail!("patch touches {path} more than once");
    }
    Ok(())
}

fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String> {
    if hunks.is_empty() {
        return Ok(original.to_string());
    }

    let (source_lines, final_newline) = split_lines(original);
    let mut output = Vec::new();
    let mut cursor = 0usize;

    for hunk in hunks {
        let old_lines = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                HunkLine::Context(text) | HunkLine::Remove(text) => Some(text.clone()),
                HunkLine::Add(_) => None,
            })
            .collect::<Vec<_>>();
        let start = find_sequence(&source_lines, &old_lines, cursor)
            .ok_or_else(|| anyhow!("patch context was not found"))?;
        output.extend_from_slice(&source_lines[cursor..start]);
        for line in &hunk.lines {
            match line {
                HunkLine::Context(text) | HunkLine::Add(text) => output.push(text.clone()),
                HunkLine::Remove(_) => {}
            }
        }
        cursor = start + old_lines.len();
    }
    output.extend_from_slice(&source_lines[cursor..]);

    Ok(join_lines(&output, final_newline))
}

fn find_sequence(source: &[String], needle: &[String], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(source.len()));
    }
    if needle.len() > source.len() || start > source.len().saturating_sub(needle.len()) {
        return None;
    }
    (start..=source.len().saturating_sub(needle.len()))
        .find(|candidate| source[*candidate..candidate + needle.len()] == *needle)
}

fn split_lines(text: &str) -> (Vec<String>, bool) {
    if text.is_empty() {
        return (Vec::new(), false);
    }
    (
        text.lines().map(str::to_string).collect(),
        text.ends_with('\n'),
    )
}

fn join_lines(lines: &[String], final_newline: bool) -> String {
    let mut text = lines.join("\n");
    if final_newline && !lines.is_empty() {
        text.push('\n');
    }
    text
}

fn apply_changes(changes: &[PlannedChange]) -> Result<()> {
    for change in changes {
        match (&change.source_path, &change.target_path) {
            (None, Some(target)) => write_file(target, &change.after)?,
            (Some(source), None) => {
                fs::remove_file(source)
                    .with_context(|| format!("failed to delete {}", source.display()))?;
            }
            (Some(source), Some(target)) if source == target => write_file(target, &change.after)?,
            (Some(source), Some(target)) => {
                write_file(target, &change.after)?;
                fs::remove_file(source)
                    .with_context(|| format!("failed to delete {}", source.display()))?;
            }
            (None, None) => {}
        }
    }
    Ok(())
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::apply_patch;
    use crate::mutation::FileChangeKind;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn apply_patch_prevalidates_every_file_before_writing() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join("good.txt"), "old\n").unwrap();
        fs::write(workspace.join("bad.txt"), "actual\n").unwrap();

        let error = apply_patch(
            &workspace,
            "*** Begin Patch\n*** Update File: good.txt\n@@\n-old\n+new\n*** Update File: bad.txt\n@@\n-expected\n+new\n*** End Patch\n",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("failed to apply patch to bad.txt")
        );
        assert_eq!(
            fs::read_to_string(workspace.join("good.txt")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn apply_patch_rejects_parent_traversal() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();

        let error = apply_patch(
            &workspace,
            "*** Begin Patch\n*** Add File: ../escape.txt\n+pwned\n*** End Patch\n",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("parent directory traversal is not allowed")
        );
    }

    #[test]
    fn apply_patch_deletes_files() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join("gone.txt"), "bye\n").unwrap();

        let result = apply_patch(
            &workspace,
            "*** Begin Patch\n*** Delete File: gone.txt\n*** End Patch\n",
        )
        .unwrap();

        let mutation = result.mutation.unwrap();
        assert_eq!(mutation.changes[0].path, "gone.txt");
        assert_eq!(mutation.changes[0].deletions, 1);
        assert!(!workspace.join("gone.txt").exists());
    }

    #[test]
    fn apply_patch_accepts_end_of_file_marker() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join("file.txt"), "old\n").unwrap();

        apply_patch(
            &workspace,
            "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n*** End of File\n*** End Patch\n",
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("file.txt")).unwrap(),
            "new\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apply_patch_rejects_delete_through_symlink() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("target.txt"), "keep me").unwrap();
        std::os::unix::fs::symlink(workspace.join("target.txt"), workspace.join("link.txt"))
            .unwrap();
        let workspace = workspace.canonicalize().unwrap();

        let error = apply_patch(
            &workspace,
            "*** Begin Patch\n*** Delete File: link.txt\n*** End Patch\n",
        )
        .unwrap_err();

        assert!(error.to_string().contains("symlink in path is not allowed"));
        assert_eq!(
            fs::read_to_string(workspace.join("target.txt")).unwrap(),
            "keep me"
        );
        assert!(workspace.join("link.txt").exists());
    }

    #[test]
    fn apply_patch_moves_and_updates_files() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join("old.txt"), "before\n").unwrap();

        let result = apply_patch(
            &workspace,
            "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n@@\n-before\n+after\n*** End Patch\n",
        )
        .unwrap();

        let mutation = result.mutation.unwrap();
        assert_eq!(mutation.changes[0].path, "old.txt");
        assert!(matches!(
            mutation.changes[0].kind,
            FileChangeKind::Update {
                move_path: Some(ref path)
            } if path == "new.txt"
        ));
        assert!(mutation.changes[0].diff.contains("-before"));
        assert!(mutation.changes[0].diff.contains("+after"));
        assert!(!workspace.join("old.txt").exists());
        assert_eq!(
            fs::read_to_string(workspace.join("new.txt")).unwrap(),
            "after\n"
        );
    }

    #[test]
    fn apply_patch_rejects_absolute_paths() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();

        let error = apply_patch(
            &workspace,
            "*** Begin Patch\n*** Add File: /tmp/escape.txt\n+pwned\n*** End Patch\n",
        )
        .unwrap_err();

        assert!(error.to_string().contains("absolute paths are not allowed"));
    }

    #[cfg(unix)]
    #[test]
    fn apply_patch_rejects_symlink_create_escape() {
        let temp = tempdir().unwrap();
        let escape = temp.path().join("escape-target");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        std::os::unix::fs::symlink(&escape, workspace.join("link")).unwrap();
        let workspace = workspace.canonicalize().unwrap();

        let error = apply_patch(
            &workspace,
            "*** Begin Patch\n*** Add File: link/file.txt\n+pwned\n*** End Patch\n",
        )
        .unwrap_err();

        assert!(error.to_string().contains("symlink in path is not allowed"));
        assert!(!escape.exists());
    }
}
