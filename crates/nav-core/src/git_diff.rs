use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use crate::mutation::{FileDiffSummary, TurnDiff, truncate_diff};

pub fn working_tree_diff(cwd: &Path) -> Result<Option<TurnDiff>> {
    if !is_git_repo(cwd)? {
        return Ok(None);
    }

    let statuses = git_statuses(cwd)?;
    let mut unified_diff = String::new();
    let mut files = Vec::new();

    if has_head(cwd)? {
        let tracked_diff = git_output(
            cwd,
            &[
                "diff",
                "--patch",
                "--no-ext-diff",
                "--no-renames",
                "HEAD",
                "--",
                ".",
            ],
        )?;
        unified_diff.push_str(&tracked_diff);
        let tracked_stats = git_output(
            cwd,
            &["diff", "--numstat", "--no-renames", "HEAD", "--", "."],
        )?;
        files.extend(parse_numstat(&tracked_stats, &statuses));
    }

    for path in untracked_files(cwd)? {
        let diff = git_output_allow_exit(
            cwd,
            &[
                "diff",
                "--no-index",
                "--patch",
                "--no-ext-diff",
                "--no-renames",
                "--",
                "/dev/null",
                &path,
            ],
            &[0, 1],
        )?;
        if !diff.is_empty() {
            if !unified_diff.is_empty() && !unified_diff.ends_with('\n') {
                unified_diff.push('\n');
            }
            unified_diff.push_str(&diff);
        }
        let stats = git_output_allow_exit(
            cwd,
            &["diff", "--no-index", "--numstat", "--", "/dev/null", &path],
            &[0, 1],
        )?;
        files.extend(parse_numstat(&stats, &statuses));
    }

    dedupe_files(&mut files);
    if unified_diff.trim().is_empty() && files.is_empty() {
        return Ok(None);
    }
    let (unified_diff, truncated) = truncate_diff(unified_diff);
    Ok(Some(TurnDiff {
        files,
        unified_diff,
        truncated,
    }))
}

fn is_git_repo(cwd: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .context("failed to run git rev-parse")?;
    Ok(output.status.success())
}

fn has_head(cwd: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(cwd)
        .output()
        .context("failed to run git rev-parse HEAD")?;
    Ok(output.status.success())
}

fn git_statuses(cwd: &Path) -> Result<BTreeMap<String, String>> {
    let raw = git_output(cwd, &["status", "--porcelain=v1", "-z"])?;
    let mut statuses = BTreeMap::new();
    for record in raw.split('\0').filter(|record| !record.is_empty()) {
        if record.len() < 4 {
            continue;
        }
        let code = &record[..2];
        let path = record[3..].to_string();
        let status = match code {
            "??" => "untracked",
            code if code.contains('A') => "added",
            code if code.contains('D') => "deleted",
            code if code.contains('M') => "modified",
            _ => "changed",
        };
        statuses.insert(path, status.to_string());
    }
    Ok(statuses)
}

fn untracked_files(cwd: &Path) -> Result<Vec<String>> {
    let raw = git_output(cwd, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    Ok(raw
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

fn parse_numstat(raw: &str, statuses: &BTreeMap<String, String>) -> Vec<FileDiffSummary> {
    raw.lines()
        .filter_map(|line| {
            let parts = line.split('\t').collect::<Vec<_>>();
            if parts.len() < 3 {
                return None;
            }
            let path = normalize_numstat_path(parts.last()?);
            Some(FileDiffSummary {
                status: statuses
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| "modified".to_string()),
                path,
                additions: parse_count(parts[0]),
                deletions: parse_count(parts[1]),
            })
        })
        .collect()
}

fn normalize_numstat_path(raw: &str) -> String {
    raw.strip_prefix("/dev/null => ")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw)
        .to_string()
}

fn parse_count(raw: &str) -> u64 {
    raw.parse().unwrap_or(0)
}

fn dedupe_files(files: &mut Vec<FileDiffSummary>) {
    let mut by_path = BTreeMap::<String, FileDiffSummary>::new();
    for file in files.drain(..) {
        by_path
            .entry(file.path.clone())
            .and_modify(|existing| {
                existing.additions += file.additions;
                existing.deletions += file.deletions;
                existing.status = file.status.clone();
            })
            .or_insert(file);
    }
    files.extend(by_path.into_values());
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String> {
    git_output_allow_exit(cwd, args, &[0])
}

fn git_output_allow_exit(cwd: &Path, args: &[&str], allowed: &[i32]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    output_to_string(output, args, allowed)
}

fn output_to_string(output: Output, args: &[&str], allowed: &[i32]) -> Result<String> {
    let code = output.status.code().unwrap_or(-1);
    if !allowed.contains(&code) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {stderr}", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::working_tree_diff;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed with {status}");
    }

    #[test]
    fn working_tree_diff_reports_tracked_and_untracked_files() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        git(&cwd, &["init"]);
        fs::write(cwd.join("tracked.txt"), "old\n").unwrap();
        git(&cwd, &["add", "tracked.txt"]);
        git(
            &cwd,
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
        fs::write(cwd.join("tracked.txt"), "new\n").unwrap();
        fs::write(cwd.join("untracked.txt"), "hello\n").unwrap();

        let diff = working_tree_diff(&cwd).unwrap().expect("diff");

        assert!(diff.files.iter().any(|file| file.path == "tracked.txt"));
        assert!(diff.files.iter().any(|file| file.path == "untracked.txt"));
        assert!(diff.unified_diff.contains("-old"));
        assert!(diff.unified_diff.contains("+new"));
        assert!(diff.unified_diff.contains("+hello"));
    }

    #[test]
    fn working_tree_diff_returns_none_outside_git_repo() {
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();

        assert!(working_tree_diff(&cwd).unwrap().is_none());
    }
}
