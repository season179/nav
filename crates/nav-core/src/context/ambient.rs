//! Turn-local ambient context assembly.
//!
//! Ambient context is intentionally tiny and ephemeral: it is added to the
//! model input for the current user turn, but it is not written to the session
//! log and therefore is not replayed forever.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::ProjectContext;
use super::history::NAV_SYNTHETIC_MARKER_KEY;
use super::project::probe_workspace;

pub const DEFAULT_AMBIENT_CONTEXT_TOKEN_BUDGET: u64 = 256;

const MAX_AMBIENT_DIR_ENTRIES: usize = 16;
const MAX_AMBIENT_DIR_SCAN: usize = 64;

pub(crate) fn build_ambient_context(
    cwd: &Path,
    project: Option<&ProjectContext>,
    token_budget: u64,
) -> Option<String> {
    if token_budget == 0 {
        return None;
    }

    let text = render_ambient_context(cwd, project);
    let tokens = estimate_tokens(&text);
    (tokens <= token_budget).then_some(text)
}

pub(crate) fn push_ambient_context(
    input: &mut Vec<Value>,
    cwd: &Path,
    project: Option<&ProjectContext>,
    token_budget: u64,
) -> bool {
    let Some(text) = build_ambient_context(cwd, project, token_budget) else {
        return false;
    };
    input.push(json!({
        "type": "message",
        "role": "user",
        "content": text,
        NAV_SYNTHETIC_MARKER_KEY: true,
    }));
    true
}

fn render_ambient_context(cwd: &Path, project: Option<&ProjectContext>) -> String {
    let mut lines = vec![
        "Ambient context (turn-local; not a user request):".to_string(),
        format!("- os: {}", std::env::consts::OS),
        format!("- cwd: {}", cwd.display()),
    ];

    // Probe the workspace fresh per turn instead of reusing the
    // `ProjectContext` snapshot captured at startup — branch and dirtiness
    // can both change between turns (commits, checkouts, edits), and
    // shipping a stale "git: main, clean" would mislead the model into
    // assuming the worktree is unchanged.
    if project.is_some_and(|p| p.workspace.is_repo) {
        lines.push(format!("- git: {}", workspace_summary(cwd)));
    } else if project.is_some() {
        lines.push("- git: not a git repo".to_string());
    }

    lines.push(format!("- cwd entries: {}", shallow_dir_summary(cwd)));
    lines.join("\n")
}

fn workspace_summary(cwd: &Path) -> String {
    let status = probe_workspace(cwd);
    if !status.is_repo {
        return "not a git repo".to_string();
    }

    let head = status.branch.as_deref().unwrap_or("unknown head");
    let cleanliness = if status.dirty { "dirty" } else { "clean" };
    format!("{head}, {cleanliness}")
}

fn shallow_dir_summary(cwd: &Path) -> String {
    let read_dir = match fs::read_dir(cwd) {
        Ok(read_dir) => read_dir,
        Err(_) => return "(unavailable)".to_string(),
    };

    let mut entries = Vec::new();
    let mut truncated = false;
    for (idx, entry) in read_dir.enumerate() {
        if idx >= MAX_AMBIENT_DIR_SCAN {
            truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let suffix = if entry.file_type().ok().is_some_and(|kind| kind.is_dir()) {
            "/"
        } else {
            ""
        };
        entries.push(format!("{name}{suffix}"));
        if entries.len() > MAX_AMBIENT_DIR_ENTRIES {
            truncated = true;
            break;
        }
    }

    entries.sort();
    if entries.len() > MAX_AMBIENT_DIR_ENTRIES {
        entries.truncate(MAX_AMBIENT_DIR_ENTRIES);
    }

    if entries.is_empty() {
        return if truncated {
            "(no visible entries in shallow scan)".to_string()
        } else {
            "(empty)".to_string()
        };
    }

    let mut out = entries.join(", ");
    if truncated {
        out.push_str(", ...");
    }
    out
}

fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count();
    if chars == 0 {
        return 0;
    }
    let char_estimate = (chars as u64).div_ceil(4);
    let word_floor = text.split_whitespace().count() as u64;
    char_estimate.max(word_floor).max(1)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::context::WorkspaceStatus;

    #[test]
    fn builds_ambient_context_when_under_budget() {
        // Drop a fake `.git/HEAD` so `probe_workspace` reports the branch
        // — the ambient context now recomputes fresh per turn rather than
        // trusting the startup `ProjectContext` snapshot. Dirtiness comes
        // from a real `git status` invocation, which on a fake repo will
        // fail and collapse to `clean` — that's fine here; the dirtiness
        // codepath is covered by integration usage rather than this test.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        fs::create_dir(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join(".env"), "secret").unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::write(
            tmp.path().join(".git").join("HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        let project = ProjectContext {
            workspace: WorkspaceStatus {
                is_repo: true,
                branch: Some("ignored-snapshot".into()),
                dirty: true,
            },
            ..ProjectContext::default()
        };

        let ambient = build_ambient_context(tmp.path(), Some(&project), 256).unwrap();

        assert!(
            ambient.contains("- git: main,"),
            "expected fresh-probe to show branch from live HEAD; got:\n{ambient}"
        );
        assert!(
            !ambient.contains("ignored-snapshot"),
            "stale snapshot branch must not appear; got:\n{ambient}"
        );
        assert!(ambient.contains("Cargo.toml"));
        assert!(ambient.contains("src/"));
        assert!(!ambient.contains(".env"));
    }

    #[test]
    fn ambient_context_recomputes_git_status_per_turn_not_from_snapshot() {
        // Regression: ambient context used to render `git: <branch>, clean`
        // from the startup ProjectContext snapshot. If the user committed,
        // switched branches, or made changes between turns, the model would
        // see stale state. We now recompute on every call so the snapshot
        // shipped here is meaningless — the rendered ambient text must
        // reflect the live filesystem, not the snapshot.
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();
        fs::write(
            tmp.path().join(".git").join("HEAD"),
            "ref: refs/heads/feature-branch\n",
        )
        .unwrap();
        // Snapshot says we're on a different branch and clean.
        let stale = ProjectContext {
            workspace: WorkspaceStatus {
                is_repo: true,
                branch: Some("stale-snapshot-branch".into()),
                dirty: false,
            },
            ..ProjectContext::default()
        };
        let ambient = build_ambient_context(tmp.path(), Some(&stale), 256).unwrap();
        assert!(
            ambient.contains("feature-branch"),
            "ambient must reflect the live HEAD, not the snapshot; got:\n{ambient}"
        );
        assert!(
            !ambient.contains("stale-snapshot-branch"),
            "ambient must NOT surface the stale snapshot branch; got:\n{ambient}"
        );
    }

    #[test]
    fn omits_ambient_context_when_over_budget() {
        let tmp = TempDir::new().unwrap();

        assert!(build_ambient_context(tmp.path(), Some(&ProjectContext::default()), 1).is_none());
    }

    #[test]
    fn stable_wording_for_empty_workspace() {
        let tmp = TempDir::new().unwrap();
        let ambient = build_ambient_context(tmp.path(), Some(&ProjectContext::default()), 256)
            .expect("ambient context should fit");

        assert_eq!(
            ambient,
            format!(
                "Ambient context (turn-local; not a user request):\n- os: {}\n- cwd: {}\n- git: not a git repo\n- cwd entries: (empty)",
                std::env::consts::OS,
                tmp.path().display()
            )
        );
    }

    #[test]
    fn push_ambient_context_adds_turn_local_message() {
        let tmp = TempDir::new().unwrap();
        let mut input = Vec::new();

        assert!(push_ambient_context(
            &mut input,
            tmp.path(),
            Some(&ProjectContext::default()),
            256,
        ));

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        assert!(
            input[0]["content"]
                .as_str()
                .unwrap()
                .starts_with("Ambient context")
        );
    }
}
