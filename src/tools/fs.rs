use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
use tokio::process::Command;

fn resolve_inside(root: &Path, requested: &str) -> Result<PathBuf> {
    // coding agents should not freely read or edit the whole machine. This
    // demo restricts path tools to relative paths under the current workspace.
    debug_assert_is_canonical(root);
    let path = relative_path(requested)?;
    let resolved = root.join(path);
    let resolved = resolved
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", resolved.display()))?;
    if !resolved.starts_with(root) {
        bail!("path escapes workspace: {}", resolved.display());
    }
    Ok(resolved)
}

fn resolve_create_path(root: &Path, requested: &str) -> Result<PathBuf> {
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

fn relative_path(requested: &str) -> Result<&Path> {
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

pub(super) fn read_file(cwd: &Path, path: &str) -> Result<String> {
    let path = resolve_inside(cwd, path)?;
    if path.is_dir() {
        bail!("{} is a directory", path.display());
    }
    fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))
}

pub(super) fn list_files(cwd: &Path, path: &str) -> Result<String> {
    let path = resolve_inside(cwd, path)?;
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

pub(super) fn edit_file(cwd: &Path, path: &str, old_str: &str, new_str: &str) -> Result<String> {
    if old_str.is_empty() {
        let path = resolve_create_path(cwd, path)?;
        if path.exists() {
            bail!("{} already exists", path.display());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, new_str)?;
        return Ok(format!("created {}", path.display()));
    }

    let path = resolve_inside(cwd, path)?;
    let original =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    // exact replacement is safer for a teaching agent than fuzzy patching.
    // Requiring one match prevents accidental broad edits.
    let matches = original.matches(old_str).take(2).count();
    if matches != 1 {
        bail!("expected exactly one match for old_str, found {matches}");
    }
    let updated = original.replacen(old_str, new_str, 1);
    fs::write(&path, updated)?;
    Ok(format!("edited {}", path.display()))
}

pub(super) async fn code_search(cwd: &Path, pattern: &str, path: &str) -> Result<String> {
    let path = resolve_inside(cwd, path)?;
    let output = Command::new("rg")
        .arg("--line-number")
        .arg("--no-heading")
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
    use super::{edit_file, read_file};
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

        let error = edit_file(&workspace, "link/file.txt", "", "pwned").unwrap_err();

        assert!(error.to_string().contains("symlink in path is not allowed"));
        assert!(!escape.exists());
    }

    #[test]
    fn create_file_writes_new_nested_file() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();

        let result = edit_file(&workspace, "subdir/file.txt", "", "hello").unwrap();

        assert!(result.contains("created"));
        assert_eq!(
            fs::read_to_string(workspace.join("subdir/file.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn create_file_rejects_absolute_paths() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();

        let error = edit_file(&workspace, "/etc/passwd", "", "pwned").unwrap_err();

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

        let error = read_file(&workspace, "link").unwrap_err();

        assert!(error.to_string().contains("path escapes workspace"));
    }

    #[test]
    fn create_file_rejects_existing_path() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join("exists.txt"), "old").unwrap();

        let error = edit_file(&workspace, "exists.txt", "", "new").unwrap_err();

        assert!(error.to_string().contains("already exists"));
    }

    #[test]
    fn parent_traversal_is_rejected() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();

        let error = edit_file(&workspace, "../escape.txt", "", "x").unwrap_err();

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

        let error = read_file(&workspace, "").unwrap_err();

        assert!(error.to_string().contains("path is required"));
    }

    #[test]
    fn edit_file_replaces_exactly_one_match() {
        let temp = tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        fs::write(workspace.join("file.txt"), "hello world").unwrap();

        edit_file(&workspace, "file.txt", "world", "Season").unwrap();

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

        let missing = edit_file(&workspace, "file.txt", "three", "x").unwrap_err();
        let repeated = edit_file(&workspace, "file.txt", "two", "x").unwrap_err();

        assert!(missing.to_string().contains("expected exactly one match"));
        assert!(repeated.to_string().contains("expected exactly one match"));
    }
}
