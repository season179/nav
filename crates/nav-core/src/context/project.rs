//! Project context, settings, and workspace status discovery.
//!
//! Three concerns share one module because they share one rule: everything is
//! resolved relative to the **launch cwd**, with no upward walk to ancestor
//! directories. That mirrors the skill-discovery convention documented in
//! AGENTS.md ("Skills and filesystem boundaries"): launching `nav` in a
//! subdirectory deliberately will not pick up a sibling-of-root `AGENTS.md`
//! or `.nav/settings.json`. The reasoning is the same — predictable scoping
//! and a single source of truth for "what counts as the project."
//!
//! Discovery covers:
//!
//! - **Context files**: `AGENTS.md` and `CLAUDE.md` at the launch cwd, plus
//!   user-scope copies at `~/.agents/`. Symlink dedup by canonical path is
//!   important — in this repo `CLAUDE.md` is a symlink to `AGENTS.md`, and
//!   loading the body twice would waste tokens and confuse the model.
//! - **Settings**: `<launch_cwd>/.nav/settings.json` and `~/.nav/settings.json`,
//!   parsed into [`Settings`]. The project file overrides the user file
//!   field-by-field; both feed [`crate::cli::Args::apply_settings`] which only
//!   overwrites clap defaults.
//! - **Workspace status**: branch name (via `.git/HEAD`) and dirty-or-not
//!   (via one `git status --porcelain` invocation). Used by the TUI welcome
//!   cell and status bar, and by the NDJSON startup banner.

use crate::cli::{AuthMode, Transport};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Aggregated result of `load_project_context`. Owned by `nav-cli` and passed
/// by reference into the agent loop, the TUI, and the NDJSON banner.
#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    pub settings: Settings,
    /// Paths of the settings files that contributed to `settings`, in merge
    /// order (user first, project last). Empty if no settings file existed.
    pub settings_sources: Vec<PathBuf>,
    /// Context files in prompt-injection order (user first, project last so
    /// the project block is the most-recent anchor in the instructions).
    pub context_files: Vec<ContextFile>,
    pub workspace: WorkspaceStatus,
}

impl ProjectContext {
    /// `"main"` / `"main ✱ (dirty)"` / `None` when not in a repo. Shared by
    /// the TUI welcome cell and the headless startup banner so both stay in
    /// lock-step when the format changes.
    pub fn branch_summary(&self) -> Option<String> {
        let branch = self.workspace.branch.clone()?;
        Some(if self.workspace.dirty {
            format!("{branch} ✱ (dirty)")
        } else {
            branch
        })
    }

    /// `"AGENTS.md (project), AGENTS.md (user)"` or `None` if no files were
    /// loaded.
    pub fn context_summary(&self) -> Option<String> {
        join_summary(
            self.context_files
                .iter()
                .map(|c| format!("{} ({})", c.display_name, c.scope.as_str())),
        )
    }

    /// `".nav/settings.json (project), ~/.nav/settings.json (user)"` or `None`
    /// if no settings files were loaded. Paths under `cwd` render relative;
    /// others fall back to `~`-shortening.
    pub fn settings_summary(&self, cwd: &Path) -> Option<String> {
        join_summary(
            self.settings_sources
                .iter()
                .map(|p| format!("{} ({})", pretty_path(p, cwd), scope_for(p, cwd))),
        )
    }
}

fn join_summary<I: IntoIterator<Item = String>>(items: I) -> Option<String> {
    let parts: Vec<String> = items.into_iter().collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn pretty_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd)
        .map(|rel| rel.display().to_string())
        .unwrap_or_else(|_| shorten_home(path))
}

fn scope_for(path: &Path, cwd: &Path) -> &'static str {
    if path.starts_with(cwd) {
        "project"
    } else {
        "user"
    }
}

/// Replace the user's home prefix with `~` so deep paths stay readable. Used
/// by the TUI status bar and the NDJSON banner; lifted to `nav-core` so both
/// frontends share one implementation.
pub fn shorten_home(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}

#[derive(Debug, Clone)]
pub struct ContextFile {
    /// Canonical path, used for dedup so a CLAUDE.md symlinked to AGENTS.md
    /// only appears once.
    pub path: PathBuf,
    /// Human-friendly basename for display (e.g. "AGENTS.md"). May differ
    /// from `path.file_name()` when the source was a symlink.
    pub display_name: String,
    pub scope: ContextScope,
    pub bytes: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextScope {
    User,
    Project,
}

impl ContextScope {
    pub fn as_str(self) -> &'static str {
        match self {
            ContextScope::User => "user",
            ContextScope::Project => "project",
        }
    }
}

/// On-disk shape for `.nav/settings.json` and `~/.nav/settings.json`.
///
/// Every field is `Option<T>` so an absent key falls through to the next
/// source in the precedence chain (project → user → clap default → explicit
/// CLI flag).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub model: Option<String>,
    pub auth: Option<AuthMode>,
    pub transport: Option<Transport>,
    pub max_turns: Option<usize>,
    pub bash_timeout_secs: Option<u64>,
    /// When `true`, skip context-file discovery entirely. Useful for repos
    /// that intentionally do not want their `AGENTS.md` shipped to the model.
    pub disable_context_files: Option<bool>,
    /// Per-model context-window budget used to decide when automatic
    /// long-session compaction fires. Setting to `0` disables auto-compaction.
    pub auto_compact_token_limit: Option<u64>,
    /// Fraction of `auto_compact_token_limit` at which automatic compaction
    /// fires. Defaults to [`crate::context::compaction::DEFAULT_AUTO_COMPACT_FRACTION`].
    pub auto_compact_fraction: Option<f32>,
    /// When true, create a git stash-backed checkpoint before each normal
    /// agent turn that starts from a dirty worktree.
    pub git_checkpoints: Option<bool>,
    /// Name of a TUI theme. `default` is built in; extension themes are
    /// discovered from `.nav/extensions/*/extension.json` and
    /// `~/.nav/extensions/*/extension.json`.
    pub theme: Option<String>,
}

impl Settings {
    /// Merge `other` on top of `self`. Any `Some(_)` field on `other` wins.
    fn merge(&mut self, other: Settings) {
        self.model = other.model.or(self.model.take());
        self.auth = other.auth.or(self.auth);
        self.transport = other.transport.or(self.transport);
        self.max_turns = other.max_turns.or(self.max_turns);
        self.bash_timeout_secs = other.bash_timeout_secs.or(self.bash_timeout_secs);
        self.disable_context_files = other.disable_context_files.or(self.disable_context_files);
        self.auto_compact_token_limit = other
            .auto_compact_token_limit
            .or(self.auto_compact_token_limit);
        self.auto_compact_fraction = other.auto_compact_fraction.or(self.auto_compact_fraction);
        self.git_checkpoints = other.git_checkpoints.or(self.git_checkpoints);
        self.theme = other.theme.or(self.theme.take());
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceStatus {
    pub is_repo: bool,
    pub branch: Option<String>,
    pub dirty: bool,
}

/// Loads everything resolvable from `launch_cwd` and the user home. Never
/// fails — unreadable or malformed files log to stderr and fall back to
/// defaults so a broken `.nav/settings.json` cannot prevent `nav` from
/// starting.
pub fn load_project_context(launch_cwd: &Path) -> ProjectContext {
    let user_home = dirs::home_dir();

    // Settings: user first, project overrides.
    let mut settings = Settings::default();
    let mut settings_sources: Vec<PathBuf> = Vec::new();
    if let Some(home) = user_home.as_deref() {
        let user_path = home.join(".nav").join("settings.json");
        if let Some(parsed) = read_settings(&user_path) {
            settings.merge(parsed);
            settings_sources.push(user_path);
        }
    }
    let project_path = launch_cwd.join(".nav").join("settings.json");
    if let Some(parsed) = read_settings(&project_path) {
        settings.merge(parsed);
        settings_sources.push(project_path);
    }

    let context_files = if settings.disable_context_files.unwrap_or(false) {
        Vec::new()
    } else {
        discover_context_files(launch_cwd, user_home.as_deref())
    };

    let workspace = probe_workspace(launch_cwd);

    ProjectContext {
        settings,
        settings_sources,
        context_files,
        workspace,
    }
}

fn read_settings(path: &Path) -> Option<Settings> {
    let bytes = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            eprintln!("nav: failed to read {}: {err}", path.display());
            return None;
        }
    };
    match serde_json::from_str::<Settings>(&bytes) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            eprintln!("nav: ignoring malformed {}: {err}", path.display());
            None
        }
    }
}

/// Reads `AGENTS.md` and `CLAUDE.md` from `launch_cwd` and `~/.agents/`. The
/// returned vec is in injection order (user first, project last) and
/// canonical-path-deduped so symlinked pairs don't double up.
fn discover_context_files(launch_cwd: &Path, user_home: Option<&Path>) -> Vec<ContextFile> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out: Vec<ContextFile> = Vec::new();

    if let Some(home) = user_home {
        let user_dir = home.join(".agents");
        collect_context_in_dir(&user_dir, ContextScope::User, &mut seen, &mut out);
    }
    collect_context_in_dir(launch_cwd, ContextScope::Project, &mut seen, &mut out);

    out
}

fn collect_context_in_dir(
    dir: &Path,
    scope: ContextScope,
    seen: &mut Vec<PathBuf>,
    out: &mut Vec<ContextFile>,
) {
    // Both basenames are spec'd uppercase; we don't case-fold to avoid
    // surprising matches like `agents.md` from unrelated tooling.
    //
    // Symlink containment: any `AGENTS.md`/`CLAUDE.md` (or symlink target) is
    // injected into the model's system prompt, so a malicious repo could
    // point one at `~/.ssh/id_rsa` and exfiltrate it. We canonicalize first
    // and reject anything that escapes `dir`'s canonical form. The legit
    // CLAUDE.md→AGENTS.md sibling symlink in this repo still resolves under
    // the project root, so it remains accepted.
    let allowed_root = match dir.canonicalize() {
        Ok(p) => p,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            eprintln!("nav: failed to resolve {}: {err}", dir.display());
            return;
        }
    };
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let path = allowed_root.join(name);
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                eprintln!("nav: failed to resolve {}: {err}", path.display());
                continue;
            }
        };
        if !canonical.starts_with(&allowed_root) {
            eprintln!(
                "nav: refusing context file at {} — symlink escapes {}",
                canonical.display(),
                allowed_root.display()
            );
            continue;
        }
        if seen.iter().any(|p| p == &canonical) {
            continue;
        }
        let bytes = match fs::read_to_string(&canonical) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("nav: failed to read {}: {err}", canonical.display());
                continue;
            }
        };
        seen.push(canonical.clone());
        out.push(ContextFile {
            path: canonical,
            display_name: name.to_string(),
            scope,
            bytes,
        });
    }
}

fn probe_workspace(cwd: &Path) -> WorkspaceStatus {
    let (is_repo, branch) = git_repo_info(cwd);
    let dirty = if is_repo { git_dirty(cwd) } else { false };
    WorkspaceStatus {
        is_repo,
        branch,
        dirty,
    }
}

/// Single ancestor walk that returns `(is_repo, branch)`. Handles the
/// linked-worktree case where `.git` is a *file* containing `gitdir: <path>`
/// pointing at the real git dir (under `<main-repo>/.git/worktrees/<name>/`);
/// in that layout `HEAD` lives at the resolved path, not next to `.git`.
fn git_repo_info(cwd: &Path) -> (bool, Option<String>) {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if let Some(git_dir) = resolve_git_dir(&d.join(".git")) {
            let branch = fs::read_to_string(git_dir.join("HEAD"))
                .ok()
                .and_then(|c| branch_from_head(&c));
            return (true, branch);
        }
        dir = d.parent();
    }
    (false, None)
}

/// Returns the actual git directory for a `.git` path, or `None` if the
/// path isn't a git pointer. Plain repos have `.git/` as a directory; linked
/// worktrees have `.git` as a file `"gitdir: <path>\n"`. The path is usually
/// absolute but git's format permits relative — those resolve against the
/// directory containing the `.git` file, **not** the process cwd, so a nested
/// invocation under a worktree subdir still finds the right HEAD.
fn resolve_git_dir(git_path: &Path) -> Option<PathBuf> {
    let metadata = fs::symlink_metadata(git_path).ok()?;
    if metadata.is_dir() {
        return Some(git_path.to_path_buf());
    }
    if metadata.is_file() {
        let contents = fs::read_to_string(git_path).ok()?;
        let gitdir = PathBuf::from(contents.strip_prefix("gitdir:")?.trim());
        if gitdir.is_absolute() {
            return Some(gitdir);
        }
        return Some(git_path.parent()?.join(gitdir));
    }
    None
}

fn branch_from_head(contents: &str) -> Option<String> {
    if let Some(rest) = contents.strip_prefix("ref: refs/heads/") {
        return Some(rest.trim().to_string());
    }
    contents.trim().get(..7).map(str::to_string)
}

/// One-shot `git status --porcelain` probe. Non-empty output ⇒ dirty.
/// A non-zero exit, missing git binary, or 2-second timeout all collapse to
/// `false` — startup must never block on git.
fn git_dirty(cwd: &Path) -> bool {
    let mut child = match Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    // Bounded wait so a slow filesystem can't stall the TUI launch.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return false;
                }
                break;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    }
    let output = match child.wait_with_output() {
        Ok(out) => out,
        Err(_) => return false,
    };
    !output.stdout.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn empty_dir_returns_defaults() {
        let tmp = TempDir::new().unwrap();
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert!(ctx.context_files.is_empty());
        assert!(ctx.settings_sources.is_empty());
        assert_eq!(ctx.settings, Settings::default());
        assert!(!ctx.workspace.is_repo);
        assert!(!ctx.workspace.dirty);
    }

    #[test]
    fn loads_agents_md_in_cwd() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("AGENTS.md"), "project guidance\n");
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert_eq!(ctx.context_files.len(), 1);
        let cf = &ctx.context_files[0];
        assert_eq!(cf.display_name, "AGENTS.md");
        assert_eq!(cf.scope, ContextScope::Project);
        assert_eq!(cf.bytes, "project guidance\n");
    }

    #[test]
    fn dedupes_claude_symlink_to_agents() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("AGENTS.md"), "shared body");
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.path().join("AGENTS.md"), tmp.path().join("CLAUDE.md"))
            .unwrap();
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert_eq!(ctx.context_files.len(), 1);
        assert_eq!(ctx.context_files[0].bytes, "shared body");
    }

    #[test]
    #[cfg(unix)]
    fn refuses_symlink_that_escapes_project_root() {
        let outside = TempDir::new().unwrap();
        write(&outside.path().join("secret"), "leaked secret");
        let tmp = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), tmp.path().join("AGENTS.md"))
            .unwrap();
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert!(
            ctx.context_files.is_empty(),
            "expected escape symlink to be refused, got {:?}",
            ctx.context_files
        );
    }

    #[test]
    fn detects_branch_in_linked_worktree_layout() {
        // git's linked-worktree layout: <cwd>/.git is a *file* pointing to
        // a per-worktree dir that holds the real HEAD.
        let tmp = TempDir::new().unwrap();
        let worktree_meta = tmp.path().join(".git-real").join("worktrees").join("wt1");
        fs::create_dir_all(&worktree_meta).unwrap();
        write(&worktree_meta.join("HEAD"), "ref: refs/heads/feature/x\n");
        write(
            &tmp.path().join(".git"),
            &format!("gitdir: {}\n", worktree_meta.display()),
        );
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert!(ctx.workspace.is_repo);
        assert_eq!(ctx.workspace.branch.as_deref(), Some("feature/x"));
    }

    #[test]
    fn relative_gitdir_resolves_against_dot_git_parent() {
        // git permits — and some tooling writes — relative `gitdir:` lines.
        // They must resolve against the directory containing `.git`, not the
        // process cwd, otherwise `nav` started in a subdirectory of a worktree
        // would silently lose branch info.
        let tmp = TempDir::new().unwrap();
        let worktree_meta = tmp.path().join(".git-real").join("worktrees").join("wt1");
        fs::create_dir_all(&worktree_meta).unwrap();
        write(&worktree_meta.join("HEAD"), "ref: refs/heads/feature/rel\n");
        write(
            &tmp.path().join(".git"),
            "gitdir: .git-real/worktrees/wt1\n",
        );
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert_eq!(ctx.workspace.branch.as_deref(), Some("feature/rel"));
    }

    #[test]
    fn detects_branch_in_plain_git_dir() {
        let tmp = TempDir::new().unwrap();
        write(
            &tmp.path().join(".git").join("HEAD"),
            "ref: refs/heads/main\n",
        );
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert!(ctx.workspace.is_repo);
        assert_eq!(ctx.workspace.branch.as_deref(), Some("main"));
    }

    #[test]
    fn loads_distinct_agents_and_claude() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("AGENTS.md"), "a");
        write(&tmp.path().join("CLAUDE.md"), "c");
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert_eq!(ctx.context_files.len(), 2);
    }

    #[test]
    fn user_context_then_project_context() {
        let tmp_home = TempDir::new().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        write(&tmp_home.path().join(".agents").join("AGENTS.md"), "u");
        write(&tmp_cwd.path().join("AGENTS.md"), "p");
        let ctx = load_project_context_with_home(tmp_cwd.path(), Some(tmp_home.path()));
        assert_eq!(ctx.context_files.len(), 2);
        assert_eq!(ctx.context_files[0].scope, ContextScope::User);
        assert_eq!(ctx.context_files[1].scope, ContextScope::Project);
    }

    #[test]
    fn project_settings_override_user_settings() {
        let tmp_home = TempDir::new().unwrap();
        let tmp_cwd = TempDir::new().unwrap();
        write(
            &tmp_home.path().join(".nav").join("settings.json"),
            r#"{"model":"u","max_turns":99,"theme":"night"}"#,
        );
        write(
            &tmp_cwd.path().join(".nav").join("settings.json"),
            r#"{"model":"p"}"#,
        );
        let ctx = load_project_context_with_home(tmp_cwd.path(), Some(tmp_home.path()));
        assert_eq!(ctx.settings.model.as_deref(), Some("p"));
        assert_eq!(ctx.settings.max_turns, Some(99));
        assert_eq!(ctx.settings.theme.as_deref(), Some("night"));
        assert_eq!(ctx.settings_sources.len(), 2);
    }

    #[test]
    fn malformed_settings_falls_back_to_defaults() {
        let tmp = TempDir::new().unwrap();
        write(
            &tmp.path().join(".nav").join("settings.json"),
            "{ not valid json",
        );
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert_eq!(ctx.settings, Settings::default());
        assert!(ctx.settings_sources.is_empty());
    }

    #[test]
    fn disable_context_files_skips_discovery() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("AGENTS.md"), "should be ignored");
        write(
            &tmp.path().join(".nav").join("settings.json"),
            r#"{"disable_context_files":true}"#,
        );
        let ctx = load_project_context_with_home(tmp.path(), None);
        assert!(ctx.context_files.is_empty());
        assert_eq!(ctx.settings.disable_context_files, Some(true));
    }

    #[test]
    fn rejects_unknown_settings_keys() {
        let tmp = TempDir::new().unwrap();
        write(
            &tmp.path().join(".nav").join("settings.json"),
            r#"{"banana":true}"#,
        );
        let ctx = load_project_context_with_home(tmp.path(), None);
        // Falls back to default because deny_unknown_fields makes the parse fail.
        assert_eq!(ctx.settings, Settings::default());
    }

    /// Test helper that lets us inject a fake `$HOME` so tests don't read the
    /// developer's real `~/.nav/` or `~/.agents/`.
    fn load_project_context_with_home(launch_cwd: &Path, home: Option<&Path>) -> ProjectContext {
        let mut settings = Settings::default();
        let mut settings_sources: Vec<PathBuf> = Vec::new();
        if let Some(home) = home {
            let user_path = home.join(".nav").join("settings.json");
            if let Some(parsed) = read_settings(&user_path) {
                settings.merge(parsed);
                settings_sources.push(user_path);
            }
        }
        let project_path = launch_cwd.join(".nav").join("settings.json");
        if let Some(parsed) = read_settings(&project_path) {
            settings.merge(parsed);
            settings_sources.push(project_path);
        }
        let context_files = if settings.disable_context_files.unwrap_or(false) {
            Vec::new()
        } else {
            discover_context_files(launch_cwd, home)
        };
        let workspace = probe_workspace(launch_cwd);
        ProjectContext {
            settings,
            settings_sources,
            context_files,
            workspace,
        }
    }
}
