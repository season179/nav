//! Path resolution + the workspace-escape guard shared by the path tools.

use std::path::{Component, Path, PathBuf};

use super::ToolError;
use super::worktree::{PathRewrite, rewrite_absolute_path};

/// Resolve `path` (relative to `cwd`, or absolute) and reject anything that
/// escapes the workspace rooted at `cwd`.
///
/// Resolution is lexical (`..`/`.` are collapsed without touching the
/// filesystem) so it works for files that don't exist yet (e.g. `write`). This
/// guards the common foot-gun — `../../etc/passwd` — but does not chase
/// symlinks; under the trusted-local posture that is an accepted gap.
pub fn resolve_in_cwd(cwd: &Path, path: &str) -> Result<PathBuf, ToolError> {
    let root = normalize(cwd);
    let candidate = if Path::new(path).is_absolute() {
        match rewrite_absolute_path(&root, Path::new(path)) {
            PathRewrite::Path(path) => normalize(&path),
            PathRewrite::Block(message) => return Err(ToolError::new(message)),
        }
    } else {
        normalize(&root.join(path))
    };

    if candidate.starts_with(&root) {
        Ok(candidate)
    } else {
        Err(ToolError::new(format!(
            "path escapes the workspace: {path}"
        )))
    }
}

/// Collapse `.` and `..` components lexically. Absolute roots are preserved.
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

/// Display `path` relative to `base` when possible, else its full form.
pub fn display_relative(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_relative_path_inside_the_workspace() {
        let cwd = Path::new("/work/space");
        let resolved = resolve_in_cwd(cwd, "src/main.rs").expect("inside workspace");
        assert_eq!(resolved, PathBuf::from("/work/space/src/main.rs"));
    }

    #[test]
    fn rejects_a_relative_escape() {
        let cwd = Path::new("/work/space");
        assert!(resolve_in_cwd(cwd, "../secret").is_err());
        assert!(resolve_in_cwd(cwd, "src/../../secret").is_err());
    }

    #[test]
    fn rejects_an_absolute_path_outside_the_workspace() {
        let cwd = Path::new("/work/space");
        assert!(resolve_in_cwd(cwd, "/etc/passwd").is_err());
    }

    #[test]
    fn allows_an_absolute_path_inside_the_workspace() {
        let cwd = Path::new("/work/space");
        let resolved = resolve_in_cwd(cwd, "/work/space/a.txt").expect("inside");
        assert_eq!(resolved, PathBuf::from("/work/space/a.txt"));
    }
}
