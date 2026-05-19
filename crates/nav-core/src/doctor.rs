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
use crate::project::{ProjectContext, shorten_home};
use crate::session::resolved_db_path;
use clap::ValueEnum;
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
}

impl DoctorReport {
    /// True when at least one row failed. `nav doctor` uses this as the
    /// process exit code so the command is scriptable.
    pub fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|c| matches!(c.status, DoctorStatus::Fail))
    }

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

struct DoctorBuilder {
    checks: Vec<DoctorCheck>,
}

impl DoctorBuilder {
    fn new() -> Self {
        Self { checks: Vec::new() }
    }

    fn ok(&mut self, group: DoctorGroup, label: impl Into<String>, detail: impl Into<String>) {
        self.push(group, label, DoctorStatus::Ok, detail);
    }

    fn warn(&mut self, group: DoctorGroup, label: impl Into<String>, detail: impl Into<String>) {
        self.push(group, label, DoctorStatus::Warn, detail);
    }

    fn fail(&mut self, group: DoctorGroup, label: impl Into<String>, detail: impl Into<String>) {
        self.push(group, label, DoctorStatus::Fail, detail);
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

    /// Look up a binary on `PATH` and record either an `ok` row with the
    /// resolved path or a `missing_status` row (warn/fail) with
    /// `missing_detail`. Collapses the repeated `match which → Some/None →
    /// ok/fail` triplet that doctor's runtime checks would otherwise repeat
    /// once per binary.
    fn check_on_path(
        &mut self,
        group: DoctorGroup,
        label: &'static str,
        ok_template: impl FnOnce(&Path) -> String,
        missing_status: DoctorStatus,
        missing_detail: &'static str,
    ) {
        match which_on_path(label) {
            Some(path) => self.ok(group, label, ok_template(&path)),
            None => self.push(group, label, missing_status, missing_detail),
        }
    }

    fn finish(self) -> DoctorReport {
        DoctorReport {
            checks: self.checks,
        }
    }
}

/// Run every check and return the aggregated report. `cwd` is the launch
/// directory; `project` is the already-loaded project context (so checks
/// stay in lock-step with what the agent loop actually saw);
/// `install_manifest_dir` is the cargo manifest directory the *binary
/// crate* was compiled from — `nav-cli` passes its own `CARGO_MANIFEST_DIR`
/// here so doctor reports the same path `nav update` will pass to `cargo
/// install --path`. Reading the env macro inside this `nav-core` module
/// would instead report `crates/nav-core`, which is not what gets
/// installed.
pub fn run(
    args: &Args,
    cwd: &Path,
    project: &ProjectContext,
    install_manifest_dir: &str,
) -> DoctorReport {
    let mut b = DoctorBuilder::new();
    check_runtime(&mut b, args);
    check_auth(&mut b, args);
    check_storage(&mut b, args);
    check_project(&mut b, cwd, project);
    check_install(&mut b, install_manifest_dir);
    b.finish()
}

// ── runtime ─────────────────────────────────────────────────────────

fn check_runtime(b: &mut DoctorBuilder, args: &Args) {
    // `rg` is invoked by the `code_search` tool and nothing in Cargo.toml
    // surfaces that dependency. A missing binary turns every search call
    // into a confusing "command not found" trace, so doctor flags it loud.
    b.check_on_path(
        DoctorGroup::Runtime,
        "rg",
        |p| format!("ripgrep on PATH at {}", p.display()),
        DoctorStatus::Fail,
        "ripgrep not on PATH — install with `brew install ripgrep` or your distro equivalent",
    );
    b.check_on_path(
        DoctorGroup::Runtime,
        "cargo",
        |p| format!("cargo on PATH at {}", p.display()),
        DoctorStatus::Warn,
        "cargo not on PATH — `nav update` will fail until rustup/cargo is installed",
    );
    if cfg!(target_os = "macos") {
        b.check_on_path(
            DoctorGroup::Runtime,
            "sandbox-exec",
            |p| format!("Seatbelt available at {}", p.display()),
            DoctorStatus::Fail,
            "sandbox-exec missing — macOS sandbox cannot be enforced",
        );
    }

    let sandbox_label = value_enum_label(args.sandbox);
    let bypassing = args.dangerously_bypass_approvals_and_sandbox;
    let status = if bypassing || matches!(args.sandbox, SandboxMode::DangerFullAccess) {
        DoctorStatus::Warn
    } else {
        DoctorStatus::Ok
    };
    let detail = if bypassing {
        format!("{sandbox_label} (--dangerously-bypass-approvals-and-sandbox active)")
    } else {
        sandbox_label.clone()
    };
    b.push(DoctorGroup::Runtime, "sandbox mode", status, detail);
}

/// Reuse clap's `kebab-case` rename for `SandboxMode` / `AuthMode` so a new
/// variant gets the right kebab name automatically — no second source of
/// truth in this module. `to_possible_value()` owns the underlying buffer,
/// so the value has to be returned as an owned `String`.
fn value_enum_label<V: ValueEnum>(mode: V) -> String {
    mode.to_possible_value()
        .expect("type derives ValueEnum with one PossibleValue per variant")
        .get_name()
        .to_string()
}

// ── auth ────────────────────────────────────────────────────────────

fn check_auth(b: &mut DoctorBuilder, args: &Args) {
    b.ok(
        DoctorGroup::Auth,
        "active mode",
        format!("--auth {}", value_enum_label(args.auth)),
    );
    match crate::auth::load_auth(args) {
        Ok(config) => b.ok(
            DoctorGroup::Auth,
            "credential",
            redacted_summary(args, &config),
        ),
        Err(err) => b.fail(DoctorGroup::Auth, "credential", format!("{err:#}")),
    }
}

fn redacted_summary(args: &Args, config: &AuthConfig) -> String {
    let endpoint = &config.http_base_url;
    let bearer_len = config.bearer.len();
    let suffix_start = config.bearer.len().saturating_sub(4);
    let suffix: String = config.bearer[suffix_start..].chars().collect();
    let credential = match args.auth {
        AuthMode::ApiKey => "OPENAI_API_KEY resolved",
        AuthMode::Chatgpt => "ChatGPT OAuth token resolved",
    };
    format!("{credential} (len {bearer_len}, ends …{suffix}); endpoint {endpoint}")
}

// ── storage ─────────────────────────────────────────────────────────

fn check_storage(b: &mut DoctorBuilder, args: &Args) {
    let resolved = match resolved_db_path(args.db_path.clone()) {
        Ok(path) => path,
        Err(err) => {
            b.fail(
                DoctorGroup::Storage,
                "db path",
                format!("could not resolve session DB path: {err:#}"),
            );
            return;
        }
    };
    b.ok(DoctorGroup::Storage, "db path", shorten_home(&resolved));
    let parent = resolved.parent().unwrap_or(Path::new("."));
    let parent_display = shorten_home(parent);
    match write_probe(parent) {
        Ok(()) => b.ok(
            DoctorGroup::Storage,
            "writable",
            format!("{parent_display} is writable"),
        ),
        Err(err) => b.fail(
            DoctorGroup::Storage,
            "writable",
            format!("cannot write under {parent_display}: {err}"),
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
    b.ok(DoctorGroup::Project, "cwd", shorten_home(cwd));

    if project.workspace.is_repo {
        let summary = project
            .branch_summary()
            .unwrap_or_else(|| "(detached HEAD)".to_string());
        b.ok(DoctorGroup::Project, "git", summary);
    } else {
        b.warn(DoctorGroup::Project, "git", "not a git repository");
    }

    match project.context_summary() {
        Some(summary) => b.ok(DoctorGroup::Project, "context files", summary),
        None => b.warn(
            DoctorGroup::Project,
            "context files",
            "no AGENTS.md or CLAUDE.md discovered",
        ),
    }

    let settings_detail = project
        .settings_summary(cwd)
        .unwrap_or_else(|| "no .nav/settings.json found (project or user)".to_string());
    b.ok(DoctorGroup::Project, "settings", settings_detail);
}

// ── install ─────────────────────────────────────────────────────────

fn check_install(b: &mut DoctorBuilder, manifest_dir: &str) {
    b.ok(
        DoctorGroup::Install,
        "version",
        format!("nav {}", env!("CARGO_PKG_VERSION")),
    );
    b.check_on_path(
        DoctorGroup::Install,
        "nav",
        |p| shorten_home(p),
        DoctorStatus::Warn,
        "nav not on PATH — running from cargo target dir is fine, otherwise add cargo's bin dir to PATH",
    );

    if Path::new(manifest_dir).exists() {
        b.ok(DoctorGroup::Install, "manifest dir", manifest_dir);
    } else {
        b.fail(
            DoctorGroup::Install,
            "manifest dir",
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
        };
        let text = report.render_text();
        assert!(text.contains("runtime\n  [ok] rg — /opt/homebrew/bin/rg"));
        assert!(text.contains("auth\n  [warn] active mode — --auth chatgpt"));
    }

    #[test]
    fn has_failures_when_any_check_fails() {
        let mut b = DoctorBuilder::new();
        b.ok(DoctorGroup::Runtime, "x", "ok");
        b.fail(DoctorGroup::Runtime, "y", "boom");
        assert!(b.finish().has_failures());
    }

    #[test]
    fn has_failures_false_when_only_warns() {
        let mut b = DoctorBuilder::new();
        b.warn(DoctorGroup::Runtime, "x", "shrug");
        assert!(!b.finish().has_failures());
    }

    #[test]
    fn run_against_empty_dir_does_not_panic_and_flags_auth() {
        // No git, no AGENTS.md, no settings, and a poisoned codex_home so
        // the auth probe deliberately fails. The full pipeline must still
        // produce a report instead of panicking.
        let tmp = TempDir::new().unwrap();
        let args = args_with_poisoned_codex_home();
        let project = ProjectContext::default();
        let report = run(&args, tmp.path(), &project, env!("CARGO_MANIFEST_DIR"));
        assert!(report.has_failures());
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
        let mut b = DoctorBuilder::new();
        check_install(&mut b, "/definitely/not/a/real/manifest/dir");
        let row = b
            .checks
            .iter()
            .find(|c| c.label == "manifest dir")
            .expect("manifest dir row");
        assert!(matches!(row.status, DoctorStatus::Fail));
        assert!(row.detail.contains("no longer exists"));
    }

    #[test]
    fn check_install_passes_for_existing_manifest_dir() {
        let tmp = TempDir::new().unwrap();
        let mut b = DoctorBuilder::new();
        check_install(&mut b, tmp.path().to_str().unwrap());
        let row = b
            .checks
            .iter()
            .find(|c| c.label == "manifest dir")
            .expect("manifest dir row");
        assert!(matches!(row.status, DoctorStatus::Ok));
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
}
