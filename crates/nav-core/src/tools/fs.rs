use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use tokio::process::Command;

use super::ToolResult;
use crate::mutation::{FileChangeKind, FileChangeSummary, MutationResult, summarize_changes};
use crate::permissions::protected::{
    PROTECTED_READ_GLOBS, is_protected_metadata_write, is_protected_read,
};

/// Post-canonicalize symlink-bypass check: refuse only when the resolved
/// path lands under protected metadata via indirection (raw request didn't
/// already name it). Direct references like `.git/config` should never
/// reach here — preflight `Block`s them — but if they do, the rule still
/// applies and we bail unconditionally.
fn ensure_not_protected_metadata(raw: &str, canonical: &Path) -> Result<()> {
    if !is_protected_metadata_write(canonical) {
        return Ok(());
    }
    if is_protected_metadata_write(raw) {
        bail!(
            "{} is protected metadata; writes are not allowed",
            canonical.display()
        );
    }
    bail!(
        "{} resolves via symlink to protected metadata; writes refused",
        canonical.display()
    );
}

/// Symlink-bypass check for protected reads. When the raw request names
/// the protected file directly (e.g. `read_file(".env")`), the preflight
/// approval flow already gated this, so honor the approval and allow.
/// Only refuse when the canonical path is protected *but* the raw request
/// isn't — that's a symlink reaching into a secret.
fn ensure_not_protected_read(raw: &str, canonical: &Path) -> Result<()> {
    if !is_protected_read(canonical) {
        return Ok(());
    }
    if is_protected_read(raw) {
        return Ok(());
    }
    bail!(
        "{} resolves via symlink to a protected secret; refused",
        canonical.display()
    );
}

pub(super) fn resolve_inside(root: &Path, requested: &str) -> Result<PathBuf> {
    resolve_under(root, &[], requested)
}

/// Resolve a requested path against `root` (for relative paths) or against
/// any of `extra_roots` (for absolute paths). Skill-aware reads pass
/// `Catalog::skill_dirs` here so the model can load files advertised in the
/// system-prompt catalog without loosening the relative-path guard.
fn resolve_under(root: &Path, extra_roots: &[PathBuf], requested: &str) -> Result<PathBuf> {
    debug_assert_is_canonical(root);
    if Path::new(requested).is_absolute() {
        let resolved = Path::new(requested)
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {requested}"))?;
        if extra_roots.iter().any(|root| resolved.starts_with(root)) {
            return Ok(resolved);
        }
        bail!(
            "absolute paths are only allowed under a known skill directory: {}",
            resolved.display()
        );
    }
    let path = relative_path(requested)?;
    let joined = root.join(path);
    let resolved = joined
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", joined.display()))?;
    if !resolved.starts_with(root) {
        bail!("path escapes workspace: {}", resolved.display());
    }
    Ok(resolved)
}

pub(super) fn resolve_create_path(root: &Path, requested: &str) -> Result<PathBuf> {
    debug_assert_is_canonical(root);
    let path = relative_path(requested)?;

    // Creates are stricter than reads: write-through-symlink is harder to make
    // race-safe, so any symlink in the create path is rejected.
    let mut checked_prefix = root.to_path_buf();
    let mut missing_suffix = PathBuf::new();
    for component in path.components() {
        if missing_suffix.as_os_str().is_empty() {
            let candidate = checked_prefix.join(component.as_os_str());
            match fs::symlink_metadata(&candidate) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    bail!("symlink in path is not allowed: {}", candidate.display());
                }
                Ok(_) => {
                    let candidate = candidate.canonicalize().with_context(|| {
                        format!("failed to canonicalize {}", candidate.display())
                    })?;
                    if !candidate.starts_with(root) {
                        bail!("path escapes workspace: {}", candidate.display());
                    }
                    checked_prefix = candidate;
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to inspect {}", candidate.display()));
                }
            }
        }
        missing_suffix.push(component.as_os_str());
    }

    let resolved = if missing_suffix.as_os_str().is_empty() {
        // Avoid adding an empty trailing component: `file.join("")` can behave
        // like `file/`, which is not the same path for an existing file.
        checked_prefix
    } else {
        checked_prefix.join(missing_suffix)
    };
    if !resolved.starts_with(root) {
        bail!("path escapes workspace: {}", resolved.display());
    }

    // This is a single-user CLI guard, not a race-proof sandbox: the path could
    // still change before fs::write. A hardened version should use atomic
    // create-new/open-no-follow APIs instead.
    Ok(resolved)
}

pub(super) fn resolve_delete_path(root: &Path, requested: &str) -> Result<PathBuf> {
    debug_assert_is_canonical(root);
    let path = relative_path(requested)?;
    let mut checked_prefix = root.to_path_buf();

    for component in path.components() {
        let candidate = checked_prefix.join(component.as_os_str());
        let metadata = fs::symlink_metadata(&candidate)
            .with_context(|| format!("failed to inspect {}", candidate.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("symlink in path is not allowed: {}", candidate.display());
        }
        let resolved = candidate
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", candidate.display()))?;
        if !resolved.starts_with(root) {
            bail!("path escapes workspace: {}", resolved.display());
        }
        checked_prefix = resolved;
    }

    Ok(checked_prefix)
}

pub(super) fn relative_path(requested: &str) -> Result<&Path> {
    if requested.is_empty() {
        bail!("path is required");
    }
    let path = Path::new(requested);
    if path.is_absolute() {
        bail!("absolute paths are not allowed");
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        bail!("parent directory traversal is not allowed");
    }
    Ok(path)
}

fn debug_assert_is_canonical(root: &Path) {
    // If canonicalize fails in debug builds, trust the caller instead of
    // panicking from the assertion helper itself.
    debug_assert_eq!(
        root,
        root.canonicalize()
            .unwrap_or_else(|_| root.to_path_buf())
            .as_path(),
        "workspace root must be canonical"
    );
}

pub(super) fn read_file(cwd: &Path, skill_dirs: &[PathBuf], path: &str) -> Result<String> {
    let raw = path;
    let path = resolve_under(cwd, skill_dirs, path)?;
    ensure_not_protected_read(raw, &path)?;
    if path.is_dir() {
        bail!("{} is a directory", path.display());
    }
    fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
}

pub(super) fn list_files(cwd: &Path, skill_dirs: &[PathBuf], path: &str) -> Result<String> {
    let path = resolve_under(cwd, skill_dirs, path)?;
    let mut entries = fs::read_dir(&path)
        .with_context(|| format!("failed to list {}", path.display()))?
        .map(|entry| {
            let entry = entry?;
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() {
                name.push('/');
            }
            Ok(name)
        })
        .collect::<Result<Vec<_>>>()?;
    entries.sort();
    Ok(serde_json::to_string_pretty(&entries)?)
}

pub(super) fn edit_file_with_metadata(
    cwd: &Path,
    path: &str,
    old_str: &str,
    new_str: &str,
) -> Result<ToolResult> {
    let raw = path;
    if old_str.is_empty() {
        let resolved = resolve_create_path(cwd, raw)?;
        ensure_not_protected_metadata(raw, &resolved)?;
        if resolved.exists() {
            bail!("{} already exists", resolved.display());
        }
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&resolved, new_str)?;
        let after_label = format!("b/{path}");
        let change = FileChangeSummary::new(
            path,
            FileChangeKind::Add,
            "",
            new_str,
            "/dev/null",
            &after_label,
        );
        let changes = vec![change];
        let summary = summarize_changes(&changes);
        let output = format!("created {}", resolved.display());
        return Ok(ToolResult::mutation(
            output,
            MutationResult { summary, changes },
        ));
    }

    let resolved = resolve_inside(cwd, raw)?;
    ensure_not_protected_metadata(raw, &resolved)?;
    ensure_not_protected_read(raw, &resolved)?;
    let original = fs::read_to_string(&resolved)
        .with_context(|| format!("failed to read {}", resolved.display()))?;
    // exact replacement is safer for a teaching agent than fuzzy patching.
    // Requiring one match prevents accidental broad edits.
    let matches = original.matches(old_str).take(2).count();
    if matches != 1 {
        bail!("expected exactly one match for old_str, found {matches}");
    }
    let updated = original.replacen(old_str, new_str, 1);
    fs::write(&resolved, &updated)?;
    let before_label = format!("a/{raw}");
    let after_label = format!("b/{raw}");
    let change = FileChangeSummary::new(
        raw,
        FileChangeKind::Update { move_path: None },
        &original,
        &updated,
        &before_label,
        &after_label,
    );
    let changes = vec![change];
    let summary = summarize_changes(&changes);
    let output = format!("edited {}", resolved.display());
    Ok(ToolResult::mutation(
        output,
        MutationResult { summary, changes },
    ))
}

pub(super) async fn code_search(
    cwd: &Path,
    skill_dirs: &[PathBuf],
    pattern: &str,
    path: &str,
) -> Result<String> {
    let raw = path;
    let path = resolve_under(cwd, skill_dirs, raw)?;
    ensure_not_protected_read(raw, &path)?;
    // Filter protected-read files out of the recursive search so
    // `code_search(.., "certs/")` doesn't surface contents of
    // `certs/server.pem` without the approval that a direct
    // `read_file("certs/server.pem")` would require.
    let mut cmd = Command::new("rg");
    cmd.arg("--line-number").arg("--no-heading");
    for glob in PROTECTED_READ_GLOBS {
        cmd.arg("--glob").arg(format!("!{glob}"));
    }
    let output = cmd
        .arg("-e")
        .arg(pattern)
        .arg(&path)
        .current_dir(cwd)
        .output()
        .await
        .context("failed to run rg")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() || output.status.code() == Some(1) {
        Ok(stdout.into_owned())
    } else {
        Ok(format!("rg failed: {stderr}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{edit_file_with_metadata, read_file};
    use std::fs;
    use tempfile::tempdir;

    #[cfg(unix)]
    #[test]
    fn create_file_rejects_broken_symlink_escape() {
        let temp = tempdir().unwrap();
        let escape = temp.path().join("escape-target");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        std::os::unix::fs::symlink(&escape, workspace.join("link")).unwrap();
        let workspace = workspace.canonicalize().unwrap();

        let error = edit_file_with_metadata(&workspace, "link/file.txt", "", "pwned").unwrap_err();

        assert!(error.to_string().contains("symlink in path is not allowed"));
        assert!(!escape.exists());
    }

    #[test]
    fn create_file_writes_new_nested_file() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();

        let result = edit_file_with_metadata(&workspace, "subdir/file.txt", "", "hello").unwrap();

        assert!(result.output.contains("created"));
        assert_eq!(
            fs::read_to_string(workspace.join("subdir/file.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn create_file_rejects_absolute_paths() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();

        let error = edit_file_with_metadata(&workspace, "/etc/passwd", "", "pwned").unwrap_err();

        assert!(error.to_string().contains("absolute paths are not allowed"));
    }

    #[cfg(unix)]
    #[test]
    fn read_file_rejects_working_symlink_escape() {
        let temp = tempdir().unwrap();
        let outside = temp.path().join("outside.txt");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("link")).unwrap();
        let workspace = workspace.canonicalize().unwrap();

        let error = read_file(&workspace, &[], "link").unwrap_err();

        assert!(error.to_string().contains("path escapes workspace"));
    }

    #[test]
    fn read_file_allows_direct_dotenv_after_approval() {
        // Approved direct reads of protected files must execute. The
        // preflight already gated this; the fs layer's symlink-bypass
        // check should not double-block.
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join(".env"), "SECRET=1").unwrap();

        let body = read_file(&workspace, &[], ".env").unwrap();
        assert_eq!(body, "SECRET=1");
    }

    #[cfg(unix)]
    #[test]
    fn read_file_rejects_symlink_to_dotenv() {
        // The preflight `is_protected_read` check runs on the raw request
        // string; a workspace symlink `cfg -> .env` slips past it. The fs
        // layer canonicalizes and must refuse.
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join(".env"), "SECRET=1").unwrap();
        std::os::unix::fs::symlink(workspace.join(".env"), workspace.join("cfg")).unwrap();

        let err = read_file(&workspace, &[], "cfg").unwrap_err();
        assert!(
            err.to_string().contains("protected secret"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn edit_file_rejects_symlink_to_git_config() {
        // Symlink bypass for edit_file: `link -> .git/config` once let
        // writes through because the preflight saw only `link`.
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::write(workspace.join(".git/config"), "[core]").unwrap();
        std::os::unix::fs::symlink(workspace.join(".git/config"), workspace.join("link"))
            .unwrap();

        let err = edit_file_with_metadata(&workspace, "link", "[core]", "x").unwrap_err();
        assert!(
            err.to_string().contains("protected metadata"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn create_file_rejects_existing_path() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join("exists.txt"), "old").unwrap();

        let error = edit_file_with_metadata(&workspace, "exists.txt", "", "new").unwrap_err();

        assert!(error.to_string().contains("already exists"));
    }

    #[test]
    fn parent_traversal_is_rejected() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();

        let error = edit_file_with_metadata(&workspace, "../escape.txt", "", "x").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("parent directory traversal is not allowed")
        );
    }

    #[test]
    fn empty_path_is_rejected() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();

        let error = read_file(&workspace, &[], "").unwrap_err();

        assert!(error.to_string().contains("path is required"));
    }

    #[test]
    fn read_file_allows_absolute_path_under_skill_dir() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();

        let skill_dir = temp.path().join("skill");
        fs::create_dir_all(&skill_dir).unwrap();
        let skill_dir = skill_dir.canonicalize().unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        fs::write(&skill_md, "skill body").unwrap();

        let body = read_file(
            &workspace,
            std::slice::from_ref(&skill_dir),
            skill_md.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(body, "skill body");
    }

    #[test]
    fn read_file_rejects_absolute_path_without_skill_root() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "secret").unwrap();

        let err = read_file(&workspace, &[], outside.to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("absolute paths are only allowed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn edit_file_replaces_exactly_one_match() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join("file.txt"), "hello world").unwrap();

        edit_file_with_metadata(&workspace, "file.txt", "world", "Season").unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("file.txt")).unwrap(),
            "hello Season"
        );
    }

    #[test]
    fn edit_file_rejects_zero_or_multiple_matches() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join("file.txt"), "one two two").unwrap();

        let missing = edit_file_with_metadata(&workspace, "file.txt", "three", "x").unwrap_err();
        let repeated = edit_file_with_metadata(&workspace, "file.txt", "two", "x").unwrap_err();

        assert!(missing.to_string().contains("expected exactly one match"));
        assert!(repeated.to_string().contains("expected exactly one match"));
    }
}
