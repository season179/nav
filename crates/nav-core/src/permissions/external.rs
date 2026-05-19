//! External-directory detection for the bash tool.
//!
//! `tools/fs.rs::resolve_inside` already enforces the workspace boundary for
//! filesystem-tool arguments. This module covers the orthogonal case: a bash
//! command that does `cd /tmp/foo && …` or that is invoked with a cwd
//! outside the workspace. Treat unresolvable paths as outside (fail-closed).

use std::path::Path;

/// True if `candidate` does not canonicalize to a descendant of `workspace`.
/// Unresolvable paths (broken symlinks, missing files) are treated as
/// **outside** so the caller fails safe — a bash `cd` to a path the agent
/// will create later still surfaces for approval.
pub fn is_outside_workspace(workspace: &Path, candidate: &Path) -> bool {
    let Ok(workspace) = workspace.canonicalize() else {
        return true;
    };
    let Ok(candidate) = candidate.canonicalize() else {
        return true;
    };
    !candidate.starts_with(workspace)
}

/// Walk a parsed pipeline looking for `cd <path>` segments and return the
/// first target that resolves outside the workspace, if any. Also unwraps
/// leading `command`/`exec`/`builtin` wrappers so `command cd /tmp` and
/// `builtin cd /tmp` are caught the same as bare `cd`.
pub fn find_external_cd<'a>(workspace: &Path, pipeline: &'a [Vec<String>]) -> Option<&'a str> {
    for argv in pipeline {
        let mut start = 0usize;
        while start < argv.len() && matches!(argv[start].as_str(), "command" | "exec" | "builtin") {
            start += 1;
            // Skip over -p/-v/-V flags that `command` accepts.
            while start < argv.len() && argv[start].starts_with('-') {
                start += 1;
            }
        }
        let mut it = argv[start..].iter();
        if it.next().map(String::as_str) != Some("cd") {
            continue;
        }
        let Some(target) = it.next() else {
            continue;
        };
        // Anything dynamic — `$VAR`, `~`, command substitution residue — is
        // treated as outside because we cannot statically know the resolved
        // path.
        if target.starts_with('$') || target.starts_with('~') {
            return Some(target);
        }
        let candidate = if Path::new(target).is_absolute() {
            std::path::PathBuf::from(target)
        } else {
            workspace.join(target)
        };
        if is_outside_workspace(workspace, &candidate) {
            return Some(target);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn descendant_is_inside() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        fs::create_dir_all(ws.join("sub")).unwrap();
        assert!(!is_outside_workspace(&ws, &ws.join("sub")));
    }

    #[test]
    fn unrelated_absolute_is_outside() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        assert!(is_outside_workspace(&ws, Path::new("/tmp")));
    }

    #[test]
    fn parent_dir_is_outside() {
        let temp = tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let ws = parent.join("workspace");
        fs::create_dir_all(&ws).unwrap();
        // workspace/../something
        assert!(is_outside_workspace(&ws, &parent.join("escape")));
    }

    #[test]
    fn unresolvable_path_is_outside() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        // /nonexistent-xyzzy can't canonicalize → fail-closed.
        assert!(is_outside_workspace(&ws, Path::new("/nonexistent-xyzzy")));
    }

    #[test]
    fn find_external_cd_returns_none_when_inside() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        fs::create_dir_all(ws.join("sub")).unwrap();
        let pipeline = vec![vec!["cd".to_string(), "sub".into()]];
        assert!(find_external_cd(&ws, &pipeline).is_none());
    }

    #[test]
    fn find_external_cd_flags_absolute_outside() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let pipeline = vec![vec!["cd".to_string(), "/tmp".into()]];
        assert_eq!(find_external_cd(&ws, &pipeline), Some("/tmp"));
    }

    #[test]
    fn find_external_cd_flags_home_expansion() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let pipeline = vec![vec!["cd".to_string(), "$HOME/proj".into()]];
        assert_eq!(find_external_cd(&ws, &pipeline), Some("$HOME/proj"));
    }

    #[test]
    fn find_external_cd_flags_tilde() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let pipeline = vec![vec!["cd".to_string(), "~/proj".into()]];
        assert_eq!(find_external_cd(&ws, &pipeline), Some("~/proj"));
    }

    #[test]
    fn find_external_cd_unwraps_command_builtin() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let pipeline = vec![vec!["command".to_string(), "cd".into(), "/tmp".into()]];
        assert_eq!(find_external_cd(&ws, &pipeline), Some("/tmp"));

        let pipeline = vec![vec!["builtin".to_string(), "cd".into(), "/tmp".into()]];
        assert_eq!(find_external_cd(&ws, &pipeline), Some("/tmp"));
    }

    #[test]
    fn find_external_cd_ignores_non_cd_segments() {
        let temp = tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let pipeline = vec![
            vec!["ls".to_string()],
            vec!["echo".to_string(), "hi".into()],
        ];
        assert!(find_external_cd(&ws, &pipeline).is_none());
    }
}
