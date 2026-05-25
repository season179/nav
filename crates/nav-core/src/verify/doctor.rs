//! `nav doctor` — one-screen health check covering the silent prerequisites,
//! credential resolution, on-disk storage, project context discovery, and the
//! installed binary. Every check returns a `[ok]/[warn]/[fail]` row with a
//! short detail line; a JSON variant exists for headless frontends. Exit code
//! flips to `1` when any row is `fail` so this slots cleanly into CI / setup
//! scripts without forcing them to parse the text output.
//!
//! Checks live here, not in `nav-cli`, so they can be unit-tested without
//! spinning up the full CLI binary.

use crate::cli::{Args, AuthMode, SandboxMode};
use crate::context::{ProjectContext, Settings, resolved_db_path, shorten_home};
use crate::model::auth::{AuthConfig, load_auth};
use crate::model::resolve_value::resolve_value;
use clap::ValueEnum;
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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
    Config,
    Storage,
    Project,
    Install,
}

impl DoctorGroup {
    fn header(self) -> &'static str {
        match self {
            DoctorGroup::Runtime => "runtime",
            DoctorGroup::Auth => "auth",
            DoctorGroup::Config => "config",
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
/// here so doctor reports the path the binary was built from. Reading the
/// env macro inside this `nav-core` module would instead report
/// `crates/nav-core`, which is not what was installed. Self-update
/// (`nav update`) no longer depends on this path; it is reported for
/// developers who want to rebuild from source.
pub fn run(
    args: &Args,
    cwd: &Path,
    project: &ProjectContext,
    install_manifest_dir: &str,
) -> DoctorReport {
    let mut b = DoctorBuilder::new();
    check_runtime(&mut b, args);
    check_auth(&mut b, args, &project.settings);
    check_config(&mut b, args, &project.settings);
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
        "cargo not on PATH — only needed to rebuild nav from source; `nav update` works without it",
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

fn check_auth(b: &mut DoctorBuilder, args: &Args, settings: &crate::context::Settings) {
    b.ok(
        DoctorGroup::Auth,
        "active mode",
        format!("--auth {}", value_enum_label(args.auth)),
    );
    match load_auth(args, settings) {
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
    // Take the last 4 *chars*, not bytes — byte indexing panics if a
    // non-ASCII character straddles the offset, which would crash the very
    // command meant to diagnose malformed credentials.
    let mut suffix_chars: Vec<char> = config.bearer.chars().rev().take(4).collect();
    suffix_chars.reverse();
    let suffix: String = suffix_chars.into_iter().collect();
    let credential = match args.auth {
        AuthMode::ApiKey => "OPENAI_API_KEY resolved",
        AuthMode::Chatgpt => "ChatGPT OAuth token resolved",
    };
    format!("{credential} (len {bearer_len}, ends …{suffix}); endpoint {endpoint}")
}

// ── config (providers & default_model) ────────────────────────────

/// Classify a config string into a human-readable credential source
/// description without leaking the actual value. Follows the same
/// precedence as [`resolve_value`]: `!command` → env → literal.
/// Returns `(description, resolved)` where `resolved` is true when the
/// credential could be read successfully.
fn credential_source(api_key: &str) -> (String, bool) {
    if let Some(cmd) = api_key.strip_prefix('!') {
        return match resolve_value(api_key) {
            Ok(Some(_)) => (format!("shell command `{cmd}` (resolves)"), true),
            Ok(None) => (format!("shell command `{cmd}` (empty output)"), false),
            Err(err) => (format!("shell command `{cmd}` (error: {err:#})"), false),
        };
    }
    if let Ok(val) = env::var(api_key) {
        if val.is_empty() {
            return (format!("env:{api_key} (empty)"), false);
        }
        return (format!("env:{api_key} (set)"), true);
    }
    (format!("literal (length: {})", api_key.len()), true)
}

fn check_config(b: &mut DoctorBuilder, args: &Args, settings: &Settings) {
    let Some(ref catalog) = settings.providers else {
        b.warn(
            DoctorGroup::Config,
            "providers",
            "no providers catalog configured",
        );
        check_active_path(b, args, settings);
        return;
    };

    // ── Per-provider credential status ──
    for (provider_id, provider) in catalog {
        let display_name = provider.name.as_deref().unwrap_or(provider_id);
        let label = format!("provider/{provider_id}");
        let models_suffix = if provider.models.is_empty() {
            "no models configured".to_string()
        } else {
            format!("{} model(s)", provider.models.len())
        };

        let (source, resolved) = match provider.api_key.as_deref() {
            Some(api_key) => credential_source(api_key),
            None => ("not set".to_string(), true),
        };
        let detail = format!("{display_name} — {source}; {models_suffix}");
        if resolved {
            b.ok(DoctorGroup::Config, &label, detail);
        } else {
            b.warn(DoctorGroup::Config, &label, detail);
        }
    }

    // ── default_model ──
    let Some(dm) = settings.default_model.as_deref() else {
        b.warn(
            DoctorGroup::Config,
            "default_model",
            "not set — pass --model or set default_model in settings.json",
        );
        check_active_path(b, args, settings);
        return;
    };

    let Some((provider_id, _model_key)) = dm.split_once('/') else {
        b.fail(
            DoctorGroup::Config,
            "default_model",
            format!("{dm} — not in <provider>/<model> form"),
        );
        check_active_path(b, args, settings);
        return;
    };

    let Some(provider) = catalog.get(provider_id) else {
        b.fail(
            DoctorGroup::Config,
            "default_model",
            format!("{dm} — provider `{provider_id}` not in catalog"),
        );
        check_active_path(b, args, settings);
        return;
    };

    let dm_resolved = match provider.api_key.as_deref() {
        Some(api_key) => credential_source(api_key).1,
        None => true,
    };
    if dm_resolved {
        b.ok(DoctorGroup::Config, "default_model", dm);
    } else {
        b.fail(
            DoctorGroup::Config,
            "default_model",
            format!("{dm} — provider `{provider_id}` credential is unresolvable"),
        );
    }

    check_active_path(b, args, settings);
}

/// What would nav do if invoked right now? Extracted so `check_config`
/// can call it at each early-return point without duplicating the match.
fn check_active_path(b: &mut DoctorBuilder, args: &Args, settings: &Settings) {
    match args.auth {
        AuthMode::Chatgpt => {
            b.ok(
                DoctorGroup::Config,
                "active path",
                "ChatGPT OAuth (--auth chatgpt)",
            );
        }
        AuthMode::ApiKey => {
            let sel = args.model.as_str();
            match crate::model::auth::resolve_provider(Some(sel), settings) {
                Ok(resolved) => b.ok(
                    DoctorGroup::Config,
                    "active path",
                    format!(
                        "provider `{}` → model `{}` (--auth api-key)",
                        resolved.display_name, resolved.model_id
                    ),
                ),
                Err(err) => b.warn(
                    DoctorGroup::Config,
                    "active path",
                    format!("could not resolve `{sel}`: {err:#}"),
                ),
            }
        }
    }
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
    // `:memory:` is a valid SQLite target that needs no disk write — the
    // generic writability probe would otherwise touch the launch cwd and
    // could report a spurious fail in a read-only directory.
    if resolved == Path::new(":memory:") {
        b.ok(DoctorGroup::Storage, "db path", ":memory:");
        b.ok(
            DoctorGroup::Storage,
            "writable",
            "in-memory database — no filesystem write needed",
        );
        return;
    }
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
        shorten_home,
        DoctorStatus::Warn,
        "nav not on PATH — running from cargo target dir is fine, otherwise add cargo's bin dir to PATH",
    );

    if Path::new(manifest_dir).exists() {
        b.ok(DoctorGroup::Install, "manifest dir", manifest_dir);
    } else {
        // `nav update` downloads a prebuilt tarball from GitHub Releases
        // (see `crates/nav-cli/src/upgrade.rs`) and does not touch this
        // directory, so a missing manifest dir is no longer ship-blocking —
        // only the developer rebuilding from source cares.
        b.warn(
            DoctorGroup::Install,
            "manifest dir",
            format!(
                "manifest dir {manifest_dir} no longer exists — only relevant for rebuilding from source; `nav update` is unaffected"
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
    which_in_dirs(name, env::split_paths(&path_var))
}

/// Walk `dirs` looking for `name`. Pulled out of [`which_on_path`] so tests
/// can pass a synthetic search path without mutating the process-wide
/// `PATH`, which races with anything else in the parallel test run that
/// spawns a subprocess.
fn which_in_dirs<I>(name: &str, dirs: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    for dir in dirs {
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
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Args, AuthMode, SandboxMode};
    use crate::context::ProjectContext;
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
    fn redacted_summary_handles_non_ascii_bearer() {
        // Slicing by byte offset on a non-ASCII string panics; the suffix
        // must come from a char-aware iterator instead.
        let config = AuthConfig {
            http_base_url: "https://api.openai.com/v1".into(),
            websocket_url: "wss://api.openai.com/v1/responses".into(),
            bearer: "abc😀de".into(),
        };
        let mut args = Args::test_default();
        args.auth = AuthMode::ApiKey;
        let summary = redacted_summary(&args, &config);
        assert!(summary.contains("…"));
        assert!(summary.contains("de"));
    }

    #[test]
    fn check_storage_special_cases_memory_db() {
        let mut args = Args::test_default();
        args.db_path = Some(std::path::PathBuf::from(":memory:"));
        let mut b = DoctorBuilder::new();
        check_storage(&mut b, &args);
        let storage_rows: Vec<_> = b
            .checks
            .iter()
            .filter(|c| matches!(c.group, DoctorGroup::Storage))
            .collect();
        assert_eq!(storage_rows.len(), 2);
        // Neither row probes the filesystem; both are ok.
        for row in &storage_rows {
            assert!(
                matches!(row.status, DoctorStatus::Ok),
                "row {} unexpectedly status {:?}",
                row.label,
                row.status
            );
        }
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
    fn which_returns_none_when_no_dir_contains_binary() {
        // Drive the inner helper directly with a known-empty dir so the
        // assertion doesn't race with other tests that spawn subprocesses
        // through the real $PATH.
        let tmp = TempDir::new().unwrap();
        let found = which_in_dirs(
            "definitely-not-a-real-binary",
            std::iter::once(tmp.path().to_path_buf()),
        );
        assert!(found.is_none());
    }

    #[test]
    fn which_finds_executable_when_dir_has_it() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("nav-shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
            let found = which_in_dirs("nav-shim", std::iter::once(tmp.path().to_path_buf()));
            assert_eq!(found.as_deref(), Some(bin.as_path()));
        }
        #[cfg(not(unix))]
        {
            let _ = bin;
        }
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
    fn check_install_warns_on_missing_manifest_dir_without_failing() {
        let mut b = DoctorBuilder::new();
        check_install(&mut b, "/definitely/not/a/real/manifest/dir");
        let row = b
            .checks
            .iter()
            .find(|c| c.label == "manifest dir")
            .expect("manifest dir row");
        // `nav update` works without this path, so a missing manifest dir
        // must not be ship-blocking.
        assert!(matches!(row.status, DoctorStatus::Warn));
        assert!(
            row.detail
                .contains("only relevant for rebuilding from source")
        );
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

    // ── config diagnostics ─────────────────────────────────────────

    use crate::context::{ModelConfig, ProviderConfig, built_in_providers};
    use std::collections::BTreeMap;

    fn settings_with_providers() -> Settings {
        let mut providers = built_in_providers();
        // Add a model under ollama so the active path can resolve.
        providers
            .get_mut("ollama")
            .unwrap()
            .models
            .insert("llama3".to_string(), ModelConfig::default());
        Settings {
            providers: Some(providers),
            default_model: Some("ollama/llama3".to_string()),
            ..Settings::default()
        }
    }

    #[test]
    fn config_section_lists_all_providers() {
        let mut b = DoctorBuilder::new();
        let settings = settings_with_providers();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        let config_rows: Vec<_> = b
            .checks
            .iter()
            .filter(|c| matches!(c.group, DoctorGroup::Config))
            .collect();
        // Should have: 8 built-in providers + default_model + active path
        assert_eq!(config_rows.len(), 10);
        // Spot-check that built-in providers appear.
        assert!(config_rows.iter().any(|r| r.label == "provider/openai"));
        assert!(config_rows.iter().any(|r| r.label == "provider/ollama"));
        assert!(config_rows.iter().any(|r| r.label == "provider/deepseek"));
    }

    #[test]
    fn config_shows_env_source_for_env_api_key() {
        let mut b = DoctorBuilder::new();
        let settings = settings_with_providers();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        // openai uses OPENAI_API_KEY. The source depends on whether the env
        // var is actually set in the test runner. We just verify no actual
        // key value leaks — the classification is tested separately in
        // `credential_source_classifies_*`.
        let openai_row = b
            .checks
            .iter()
            .find(|c| c.label == "provider/openai")
            .unwrap();
        // The important thing: no actual key value leaks.
        assert!(
            !openai_row.detail.contains("sk-") || openai_row.detail.contains("literal"),
            "credential leaked: {}",
            openai_row.detail
        );
    }

    #[test]
    fn config_shows_not_set_for_local_providers() {
        let mut b = DoctorBuilder::new();
        let settings = settings_with_providers();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        let ollama_row = b
            .checks
            .iter()
            .find(|c| c.label == "provider/ollama")
            .unwrap();
        assert!(
            ollama_row.detail.contains("not set"),
            "expected 'not set' for local provider, got: {}",
            ollama_row.detail
        );
        // No api_key means ok status.
        assert!(matches!(ollama_row.status, DoctorStatus::Ok));
    }

    #[test]
    fn config_literal_api_key_shows_length() {
        let mut settings = settings_with_providers();
        settings.providers.as_mut().unwrap().insert(
            "custom".to_string(),
            ProviderConfig {
                name: Some("Custom".to_string()),
                base_url: Some("https://api.custom.example/v1".to_string()),
                api_key: Some("sk-1234567890".to_string()),
                headers: None,
                models: BTreeMap::new(),
            },
        );
        let mut b = DoctorBuilder::new();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        let custom_row = b
            .checks
            .iter()
            .find(|c| c.label == "provider/custom")
            .unwrap();
        assert!(
            custom_row.detail.contains("literal (length: 13)"),
            "expected literal with length 13, got: {}",
            custom_row.detail
        );
        assert!(matches!(custom_row.status, DoctorStatus::Ok));
    }

    #[test]
    fn config_warns_on_unresolvable_shell_command() {
        let mut settings = settings_with_providers();
        settings.providers.as_mut().unwrap().insert(
            "shellprov".to_string(),
            ProviderConfig {
                name: Some("ShellProv".to_string()),
                base_url: Some("https://api.shell.example/v1".to_string()),
                api_key: Some("!false".to_string()),
                headers: None,
                models: BTreeMap::new(),
            },
        );
        let mut b = DoctorBuilder::new();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        let row = b
            .checks
            .iter()
            .find(|c| c.label == "provider/shellprov")
            .unwrap();
        assert!(
            matches!(row.status, DoctorStatus::Warn),
            "unresolvable shell command should be warn, got {:?}",
            row.status
        );
        assert!(row.detail.contains("shell command"));
    }

    #[test]
    fn config_default_model_ok_when_resolvable() {
        let mut b = DoctorBuilder::new();
        let settings = settings_with_providers();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        let dm_row = b
            .checks
            .iter()
            .find(|c| c.label == "default_model")
            .unwrap();
        assert!(matches!(dm_row.status, DoctorStatus::Ok));
        assert!(dm_row.detail.contains("ollama/llama3"));
    }

    #[test]
    fn config_default_model_fail_when_provider_unresolvable() {
        let mut settings = settings_with_providers();
        // Point default_model at a provider with a failing shell command.
        settings.providers.as_mut().unwrap().insert(
            "broken".to_string(),
            ProviderConfig {
                name: Some("Broken".to_string()),
                base_url: Some("https://api.broken.example/v1".to_string()),
                api_key: Some("!false".to_string()),
                headers: None,
                models: {
                    let mut m = BTreeMap::new();
                    m.insert("m1".to_string(), ModelConfig::default());
                    m
                },
            },
        );
        settings.default_model = Some("broken/m1".to_string());
        let mut b = DoctorBuilder::new();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        let dm_row = b
            .checks
            .iter()
            .find(|c| c.label == "default_model")
            .unwrap();
        assert!(
            matches!(dm_row.status, DoctorStatus::Fail),
            "expected fail for unresolvable default_model, got {:?}",
            dm_row.status
        );
        assert!(dm_row.detail.contains("unresolvable"));
    }

    #[test]
    fn config_default_model_warn_when_not_set() {
        let mut settings = settings_with_providers();
        settings.default_model = None;
        let mut b = DoctorBuilder::new();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        let dm_row = b
            .checks
            .iter()
            .find(|c| c.label == "default_model")
            .unwrap();
        assert!(matches!(dm_row.status, DoctorStatus::Warn));
    }

    #[test]
    fn config_active_path_shows_chatgpt_when_auth_chatgpt() {
        let mut b = DoctorBuilder::new();
        let settings = settings_with_providers();
        let mut args = Args::test_default();
        args.auth = AuthMode::Chatgpt;
        check_config(&mut b, &args, &settings);
        let path_row = b.checks.iter().find(|c| c.label == "active path").unwrap();
        assert!(matches!(path_row.status, DoctorStatus::Ok));
        assert!(path_row.detail.contains("ChatGPT"));
    }

    #[test]
    fn config_active_path_resolves_provider_for_api_key() {
        let mut b = DoctorBuilder::new();
        let settings = settings_with_providers();
        let mut args = Args::test_default();
        args.auth = AuthMode::ApiKey;
        args.model = "ollama/llama3".to_string();
        check_config(&mut b, &args, &settings);
        let path_row = b.checks.iter().find(|c| c.label == "active path").unwrap();
        assert!(matches!(path_row.status, DoctorStatus::Ok));
        // Display name uses the provider's `name` field: "Ollama (local)".
        assert!(
            path_row.detail.contains("Ollama"),
            "got: {}",
            path_row.detail
        );
    }

    #[test]
    fn config_warns_when_no_providers_catalog() {
        let settings = Settings::default();
        let mut b = DoctorBuilder::new();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        let config_rows: Vec<_> = b
            .checks
            .iter()
            .filter(|c| matches!(c.group, DoctorGroup::Config))
            .collect();
        // 1 warn (no providers) + 1 active path row.
        assert_eq!(config_rows.len(), 2);
        assert!(matches!(config_rows[0].status, DoctorStatus::Warn));
        assert!(config_rows[0].detail.contains("no providers catalog"));
    }

    #[test]
    fn config_credential_source_never_leaks_values() {
        let mut b = DoctorBuilder::new();
        let settings = settings_with_providers();
        let args = Args::test_default();
        check_config(&mut b, &args, &settings);
        let all_text: String = b
            .checks
            .iter()
            .map(|c| format!("{} {}", c.label, c.detail))
            .collect::<Vec<_>>()
            .join("\n");
        // Env var names are ok, but actual env values must not appear.
        // OPENAI_API_KEY is the env name used by the built-in openai provider.
        // The literal "OPENAI_API_KEY" is fine; a resolved sk-… value is not.
        assert!(
            !all_text.contains("sk-") || all_text.contains("literal"),
            "credential value may have leaked: {all_text}"
        );
    }

    #[test]
    fn credential_source_classifies_shell_command() {
        let (src, resolved) = credential_source("!echo secret");
        assert!(src.contains("shell command"), "got: {src}");
        assert!(src.contains("resolves"), "got: {src}");
        assert!(resolved);
    }

    #[test]
    fn credential_source_classifies_failing_shell_command() {
        let (src, resolved) = credential_source("!false");
        assert!(src.contains("shell command"), "got: {src}");
        assert!(src.contains("error"), "got: {src}");
        assert!(!resolved);
    }

    #[test]
    fn credential_source_classifies_literal() {
        let (src, resolved) = credential_source("sk-my-literal-key");
        assert!(src.contains("literal"), "got: {src}");
        assert!(src.contains("length: 17"), "got: {src}");
        assert!(resolved);
    }
}
