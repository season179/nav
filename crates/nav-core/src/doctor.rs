//! `nav doctor` — one-screen health check covering the silent prerequisites,
//! credential resolution, on-disk storage, project context discovery, and the
//! installed binary. Every check returns a `[ok]/[warn]/[fail]` row with a
//! short detail line; a JSON variant exists for headless frontends. Exit code
//! flips to `1` when any row is `fail` so this slots cleanly into CI / setup
//! scripts without forcing them to parse the text output.
//!
//! Checks live here, not in `nav-cli`, so they can be unit-tested without
//! spinning up the full CLI binary.

use crate::auth::AuthConfig;
use crate::cli::{Args, AuthMode, SandboxMode};
use crate::project::ProjectContext;
use crate::session::resolved_db_path;
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Outcome of a single doctor check. The three-state ladder mirrors common
/// CI conventions: `Ok` is silently passing, `Warn` flags something the user
/// may want to know but does not block, and `Fail` is the only state that
/// flips the process exit code to non-zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorStatus {
    Ok,
    Warn,
    Fail,
}

impl DoctorStatus {
    fn tag(self) -> &'static str {
        match self {
            DoctorStatus::Ok => "[ok]",
            DoctorStatus::Warn => "[warn]",
            DoctorStatus::Fail => "[fail]",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorGroup {
    Runtime,
    Auth,
    Storage,
    Project,
    Install,
}

impl DoctorGroup {
    fn header(self) -> &'static str {
        match self {
            DoctorGroup::Runtime => "runtime",
            DoctorGroup::Auth => "auth",
            DoctorGroup::Storage => "storage",
            DoctorGroup::Project => "project",
            DoctorGroup::Install => "install",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorCheck {
    pub group: DoctorGroup,
    pub label: String,
    pub status: DoctorStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    /// True when at least one row failed. Callers use this as the process
    /// exit code so `nav doctor` is scriptable.
    pub has_failures: bool,
}

impl DoctorReport {
    /// Render as `[ok]/[warn]/[fail] group/label — detail`, grouped by section
    /// with a one-line header per group. Mirrors what a human-grade health
    /// dashboard would print.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        let mut current: Option<DoctorGroup> = None;
        for check in &self.checks {
            if current != Some(check.group) {
                if current.is_some() {
                    out.push('\n');
                }
                out.push_str(check.group.header());
                out.push('\n');
                current = Some(check.group);
            }
            out.push_str(&format!(
                "  {} {} — {}\n",
                check.status.tag(),
                check.label,
                check.detail
            ));
        }
        out
    }
}

/// Builder used by the run function so individual checks can be unit-tested
/// in isolation against a synthetic input. The CLI just calls [`run`].
struct DoctorBuilder {
    checks: Vec<DoctorCheck>,
}

impl DoctorBuilder {
    fn new() -> Self {
        Self { checks: Vec::new() }
    }

    fn push(
        &mut self,
        group: DoctorGroup,
        label: impl Into<String>,
        status: DoctorStatus,
        detail: impl Into<String>,
    ) {
        self.checks.push(DoctorCheck {
            group,
            label: label.into(),
            status,
            detail: detail.into(),
        });
    }

    fn finish(self) -> DoctorReport {
        let has_failures = self
            .checks
            .iter()
            .any(|c| matches!(c.status, DoctorStatus::Fail));
        DoctorReport {
            checks: self.checks,
            has_failures,
        }
    }
}

/// Run every check and return the aggregated report. `cwd` is the launch
/// directory; `project` is the already-loaded project context (so checks
/// stay in lock-step with what the agent loop actually saw).
pub fn run(args: &Args, cwd: &Path, project: &ProjectContext) -> DoctorReport {
    let mut b = DoctorBuilder::new();
    check_runtime(&mut b, args);
    check_auth(&mut b, args);
    check_storage(&mut b, args);
    check_project(&mut b, cwd, project);
    check_install(&mut b);
    b.finish()
}

// ── runtime ─────────────────────────────────────────────────────────

fn check_runtime(b: &mut DoctorBuilder, args: &Args) {
    // `rg` is invoked by the `code_search` tool and nothing in Cargo.toml
    // surfaces that dependency. A missing binary turns every search call
    // into a confusing "command not found" trace, so doctor flags it loud.
    match which_on_path("rg") {
        Some(path) => b.push(
            DoctorGroup::Runtime,
            "rg",
            DoctorStatus::Ok,
            format!("ripgrep on PATH at {}", path.display()),
        ),
        None => b.push(
            DoctorGroup::Runtime,
            "rg",
            DoctorStatus::Fail,
            "ripgrep not on PATH — install with `brew install ripgrep` or your distro equivalent",
        ),
    }
    match which_on_path("cargo") {
        Some(path) => b.push(
            DoctorGroup::Runtime,
            "cargo",
            DoctorStatus::Ok,
            format!("cargo on PATH at {}", path.display()),
        ),
        None => b.push(
            DoctorGroup::Runtime,
            "cargo",
            DoctorStatus::Warn,
            "cargo not on PATH — `nav update` will fail until rustup/cargo is installed",
        ),
    }
    if cfg!(target_os = "macos") {
        match which_on_path("sandbox-exec") {
            Some(path) => b.push(
                DoctorGroup::Runtime,
                "sandbox-exec",
                DoctorStatus::Ok,
                format!("Seatbelt available at {}", path.display()),
            ),
            None => b.push(
                DoctorGroup::Runtime,
                "sandbox-exec",
                DoctorStatus::Fail,
                "sandbox-exec missing — macOS sandbox cannot be enforced",
            ),
        }
    }
    let sandbox_label = match args.sandbox {
        SandboxMode::ReadOnly => "read-only",
        SandboxMode::WorkspaceWrite => "workspace-write",
        SandboxMode::DangerFullAccess => "danger-full-access",
    };
    let status = if args.dangerously_bypass_approvals_and_sandbox
        || matches!(args.sandbox, SandboxMode::DangerFullAccess)
    {
        DoctorStatus::Warn
    } else {
        DoctorStatus::Ok
    };
    let detail = if args.dangerously_bypass_approvals_and_sandbox {
        format!("{sandbox_label} (--dangerously-bypass-approvals-and-sandbox active)")
    } else {
        sandbox_label.to_string()
    };
    b.push(DoctorGroup::Runtime, "sandbox mode", status, detail);
}

// ── auth ────────────────────────────────────────────────────────────

fn check_auth(b: &mut DoctorBuilder, args: &Args) {
    let mode = match args.auth {
        AuthMode::Chatgpt => "chatgpt",
        AuthMode::ApiKey => "api-key",
    };
    b.push(
        DoctorGroup::Auth,
        "active mode",
        DoctorStatus::Ok,
        format!("--auth {mode}"),
    );

    match crate::auth::load_auth(args) {
        Ok(config) => {
            b.push(
                DoctorGroup::Auth,
                "credential",
                DoctorStatus::Ok,
                redacted_summary(args, &config),
            );
        }
        Err(err) => {
            b.push(
                DoctorGroup::Auth,
                "credential",
                DoctorStatus::Fail,
                format!("{err:#}"),
            );
        }
    }
}

fn redacted_summary(args: &Args, config: &AuthConfig) -> String {
    let endpoint = &config.http_base_url;
    let bearer_len = config.bearer.len();
    let suffix: String = config.bearer.chars().rev().take(4).collect();
    let suffix: String = suffix.chars().rev().collect();
    match args.auth {
        AuthMode::ApiKey => {
            format!(
                "OPENAI_API_KEY resolved (len {bearer_len}, ends …{suffix}); endpoint {endpoint}"
            )
        }
        AuthMode::Chatgpt => {
            format!(
                "ChatGPT OAuth token resolved (len {bearer_len}, ends …{suffix}); endpoint {endpoint}"
            )
        }
    }
}

// ── storage ─────────────────────────────────────────────────────────

fn check_storage(b: &mut DoctorBuilder, args: &Args) {
    let resolved = match resolved_db_path(args.db_path.clone()) {
        Ok(path) => path,
        Err(err) => {
            b.push(
                DoctorGroup::Storage,
                "db path",
                DoctorStatus::Fail,
                format!("could not resolve session DB path: {err:#}"),
            );
            return;
        }
    };
    b.push(
        DoctorGroup::Storage,
        "db path",
        DoctorStatus::Ok,
        format!("{}", resolved.display()),
    );
    let parent = resolved.parent().unwrap_or(Path::new("."));
    match write_probe(parent) {
        Ok(()) => b.push(
            DoctorGroup::Storage,
            "writable",
            DoctorStatus::Ok,
            format!("{} is writable", parent.display()),
        ),
        Err(err) => b.push(
            DoctorGroup::Storage,
            "writable",
            DoctorStatus::Fail,
            format!("cannot write under {}: {err}", parent.display()),
        ),
    }
}

/// Best-effort write check: tries to create the directory, then write+delete
/// a temp file inside it. Returns `Ok(())` on success.
fn write_probe(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let probe = dir.join(format!(".nav-doctor-{}", std::process::id()));
    fs::write(&probe, b"x").map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&probe);
    Ok(())
}

// ── project ─────────────────────────────────────────────────────────

fn check_project(b: &mut DoctorBuilder, cwd: &Path, project: &ProjectContext) {
    b.push(
        DoctorGroup::Project,
        "cwd",
        DoctorStatus::Ok,
        cwd.display().to_string(),
    );
    let git_summary = if project.workspace.is_repo {
        project
            .branch_summary()
            .unwrap_or_else(|| "(detached HEAD)".to_string())
    } else {
        "not a git repository".to_string()
    };
    let git_status = if project.workspace.is_repo {
        DoctorStatus::Ok
    } else {
        DoctorStatus::Warn
    };
    b.push(DoctorGroup::Project, "git", git_status, git_summary);

    let context_detail = project
        .context_summary()
        .unwrap_or_else(|| "no AGENTS.md or CLAUDE.md discovered".to_string());
    let context_status = if project.context_files.is_empty() {
        DoctorStatus::Warn
    } else {
        DoctorStatus::Ok
    };
    b.push(
        DoctorGroup::Project,
        "context files",
        context_status,
        context_detail,
    );

    let settings_detail = project
        .settings_summary(cwd)
        .unwrap_or_else(|| "no .nav/settings.json found (project or user)".to_string());
    b.push(
        DoctorGroup::Project,
        "settings",
        DoctorStatus::Ok,
        settings_detail,
    );
}

// ── install ─────────────────────────────────────────────────────────

fn check_install(b: &mut DoctorBuilder) {
    let version = env!("CARGO_PKG_VERSION");
    b.push(
        DoctorGroup::Install,
        "version",
        DoctorStatus::Ok,
        format!("nav {version}"),
    );

    match which_on_path("nav") {
        Some(path) => b.push(
            DoctorGroup::Install,
            "resolved binary",
            DoctorStatus::Ok,
            path.display().to_string(),
        ),
        None => b.push(
            DoctorGroup::Install,
            "resolved binary",
            DoctorStatus::Warn,
            "nav not on PATH — running from cargo target dir is fine, otherwise add cargo's bin dir to PATH",
        ),
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    if Path::new(manifest_dir).exists() {
        b.push(
            DoctorGroup::Install,
            "manifest dir",
            DoctorStatus::Ok,
            manifest_dir.to_string(),
        );
    } else {
        b.push(
            DoctorGroup::Install,
            "manifest dir",
            DoctorStatus::Fail,
            format!(
                "manifest dir {manifest_dir} no longer exists — `nav update` will fail; reinstall from a current checkout"
            ),
        );
    }
}

// ── helpers ─────────────────────────────────────────────────────────

/// Return the first matching executable on `$PATH`, or `None` if not found.
/// Mirrors `which(1)` without pulling in a new crate. On Windows the call
/// site doesn't run (we only ship on macOS/Linux today), so we skip the
/// `PATHEXT` dance.
pub fn which_on_path(name: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o111 != 0;
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Resolve `cargo`'s install prefix (the `bin` dir cargo writes into for
/// `cargo install`). Used by `nav update` to detect a PATH-shim mismatch
/// where the global `nav` is shadowed by an older binary in a sibling dir.
/// Returns `$CARGO_INSTALL_ROOT/bin` or `$CARGO_HOME/bin`, falling back to
/// `~/.cargo/bin`.
pub fn cargo_install_bin_dir() -> Option<PathBuf> {
    if let Some(root) = env::var_os("CARGO_INSTALL_ROOT") {
        return Some(PathBuf::from(root).join("bin"));
    }
    if let Some(home) = env::var_os("CARGO_HOME") {
        return Some(PathBuf::from(home).join("bin"));
    }
    dirs::home_dir().map(|h| h.join(".cargo").join("bin"))
}

/// Run a freshly installed binary with `--version` and return the trimmed
/// first line of stdout. Used by `nav update` to print a `from X → Y`
/// summary that proves the reinstall actually took effect.
pub fn binary_version(bin: &Path) -> Option<String> {
    let output = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .next()?
        .trim()
        .to_string();
    // `clap` formats this as `nav 26.5.2` — strip the leading binary name so
    // callers can compare versions directly.
    Some(line.split_whitespace().last().unwrap_or(&line).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Args, AuthMode, SandboxMode};
    use crate::project::ProjectContext;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn args_with_poisoned_codex_home() -> Args {
        // load_auth fails fast when the poisoned codex_home doesn't have an
        // auth.json — that's how the integration test isolates a developer's
        // real ~/.codex from doctor's auth probe.
        let mut args = Args::test_default();
        args.auth = AuthMode::Chatgpt;
        args.sandbox = SandboxMode::WorkspaceWrite;
        args
    }

    #[test]
    fn report_renders_grouped_text() {
        let report = DoctorReport {
            checks: vec![
                DoctorCheck {
                    group: DoctorGroup::Runtime,
                    label: "rg".into(),
                    status: DoctorStatus::Ok,
                    detail: "/opt/homebrew/bin/rg".into(),
                },
                DoctorCheck {
                    group: DoctorGroup::Auth,
                    label: "active mode".into(),
                    status: DoctorStatus::Warn,
                    detail: "--auth chatgpt".into(),
                },
            ],
            has_failures: false,
        };
        let text = report.render_text();
        // Groups are headed and each row uses the [tag] label — detail format.
        assert!(text.contains("runtime\n  [ok] rg — /opt/homebrew/bin/rg"));
        assert!(text.contains("auth\n  [warn] active mode — --auth chatgpt"));
    }

    #[test]
    fn has_failures_when_any_check_fails() {
        let mut b = DoctorBuilder::new();
        b.push(DoctorGroup::Runtime, "x", DoctorStatus::Ok, "ok");
        b.push(DoctorGroup::Runtime, "y", DoctorStatus::Fail, "boom");
        let report = b.finish();
        assert!(report.has_failures);
    }

    #[test]
    fn has_failures_false_when_only_warns() {
        let mut b = DoctorBuilder::new();
        b.push(DoctorGroup::Runtime, "x", DoctorStatus::Warn, "shrug");
        let report = b.finish();
        assert!(!report.has_failures);
    }

    #[test]
    fn run_against_empty_dir_does_not_panic_and_flags_auth() {
        // No git, no AGENTS.md, no settings, and a poisoned codex_home so
        // the auth probe deliberately fails. The full pipeline must still
        // produce a report instead of panicking.
        let tmp = TempDir::new().unwrap();
        let args = args_with_poisoned_codex_home();
        let project = ProjectContext::default();
        let report = run(&args, tmp.path(), &project);
        // Auth must have failed because the poisoned codex_home has no auth.json.
        assert!(report.has_failures);
        let auth_fail = report
            .checks
            .iter()
            .find(|c| matches!(c.group, DoctorGroup::Auth) && c.label == "credential")
            .expect("expected auth credential row");
        assert!(matches!(auth_fail.status, DoctorStatus::Fail));
    }

    #[test]
    fn redacted_summary_never_includes_full_bearer() {
        let config = AuthConfig {
            http_base_url: "https://api.openai.com/v1".into(),
            websocket_url: "wss://api.openai.com/v1/responses".into(),
            bearer: "sk-this-should-never-be-shown-in-doctor-1234".into(),
        };
        let mut args = Args::test_default();
        args.auth = AuthMode::ApiKey;
        let summary = redacted_summary(&args, &config);
        assert!(!summary.contains("sk-this-should-never-be-shown"));
        assert!(summary.contains("…1234"));
        assert!(summary.contains("api.openai.com"));
    }

    #[test]
    fn which_returns_none_for_missing_binary() {
        let tmp = TempDir::new().unwrap();
        // Constrain PATH to an empty dir so the lookup deterministically misses.
        let prev = env::var_os("PATH");
        // SAFETY: tests in this module run with `cargo test --test-threads=1`
        // is not guaranteed; this is best-effort. We restore PATH below.
        unsafe { env::set_var("PATH", tmp.path()) };
        let found = which_on_path("definitely-not-a-real-binary");
        if let Some(prev) = prev {
            unsafe { env::set_var("PATH", prev) };
        } else {
            unsafe { env::remove_var("PATH") };
        }
        assert!(found.is_none());
    }

    #[test]
    fn write_probe_succeeds_on_writable_temp_dir() {
        let tmp = TempDir::new().unwrap();
        write_probe(tmp.path()).expect("temp dir must be writable");
        // The probe file is cleaned up immediately.
        let leftover: Vec<_> = fs::read_dir(tmp.path()).unwrap().collect();
        assert!(leftover.is_empty(), "probe should clean itself up");
    }

    #[test]
    fn check_install_flags_missing_manifest_dir() {
        // If CARGO_MANIFEST_DIR were stale we'd want a fail row. We can't
        // re-set the compile-time env var, but we can at least verify the
        // current manifest dir is reported as present.
        let mut b = DoctorBuilder::new();
        check_install(&mut b);
        let manifest_row = b
            .checks
            .iter()
            .find(|c| c.label == "manifest dir")
            .expect("manifest dir row");
        assert_eq!(
            manifest_row.group as u8,
            DoctorGroup::Install as u8,
            "should be in install group"
        );
    }

    #[test]
    fn binary_version_strips_clap_prefix() {
        // Synthesise a shim that prints the canonical `clap` --version line.
        let tmp = TempDir::new().unwrap();
        let shim = tmp.path().join("nav-shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(&shim, "#!/bin/sh\necho 'nav 99.0.0'\n").unwrap();
            fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
            let parsed = binary_version(&shim).unwrap();
            assert_eq!(parsed, "99.0.0");
        }
        #[cfg(not(unix))]
        {
            let _ = shim;
        }
    }

    #[test]
    fn cargo_install_bin_dir_prefers_install_root() {
        let tmp = TempDir::new().unwrap();
        let prev = env::var_os("CARGO_INSTALL_ROOT");
        unsafe { env::set_var("CARGO_INSTALL_ROOT", tmp.path()) };
        let resolved = cargo_install_bin_dir().unwrap();
        assert_eq!(resolved, tmp.path().join("bin"));
        if let Some(prev) = prev {
            unsafe { env::set_var("CARGO_INSTALL_ROOT", prev) };
        } else {
            unsafe { env::remove_var("CARGO_INSTALL_ROOT") };
        }
    }

    #[allow(dead_code)]
    fn _path_buf_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<PathBuf>();
    }
}
