//! macOS Seatbelt sandbox runner.
//!
//! Spawns the user's command under `/usr/bin/sandbox-exec` with a profile
//! assembled from `seatbelt.sbpl` plus per-invocation augmentations:
//! - one `(allow file-write* (subpath "<root>"))` per writable root
//! - one `(allow network*)` block if `network: true`
//!
//! The classifier and protected-path pre-flight in `tools/` runs before we
//! get here, so this layer is purely about hardening the spawn — nav still
//! refuses dangerous commands regardless of whether the sandbox is active.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use tokio::{process::Command, time};

use super::{SandboxOutput, SandboxRequest, SandboxRunner};
use crate::permissions::SandboxPolicy;
use crate::permissions::protected::PROTECTED_METADATA_NAMES;

/// Hard-coded path; matches codex. Resolving via `$PATH` would let a
/// poisoned PATH defeat the sandbox.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Base policy embedded at compile time.
const BASE_POLICY: &str = include_str!("seatbelt.sbpl");

pub struct SeatbeltRunner;

impl SandboxRunner for SeatbeltRunner {
    fn run<'a>(
        &'a self,
        req: SandboxRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SandboxOutput>> + Send + 'a>>
    {
        Box::pin(async move {
            let profile = build_profile(&req.policy)?;
            let mut cmd = Command::new(SANDBOX_EXEC);
            cmd.kill_on_drop(true)
                .arg("-p")
                .arg(&profile)
                .arg("sh")
                .arg("-c")
                .arg(&req.command)
                .current_dir(&req.cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let child = cmd
                .spawn()
                .with_context(|| format!("failed to spawn sandbox-exec for `{}`", req.command))?;

            let output = match time::timeout(req.timeout, child.wait_with_output()).await {
                Ok(output) => output?,
                Err(_) => bail!(
                    "command timed out after {}s: {}",
                    req.timeout.as_secs(),
                    req.command
                ),
            };

            Ok(SandboxOutput {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                status: output.status.code(),
                status_display: format!("{}", output.status),
            })
        })
    }
}

/// Assemble the full Seatbelt profile for a given policy. Public for tests.
pub fn build_profile(policy: &SandboxPolicy) -> Result<String> {
    let mut s = String::with_capacity(BASE_POLICY.len() + 256);
    s.push_str(BASE_POLICY);
    s.push('\n');
    match policy {
        SandboxPolicy::DangerFullAccess => {
            // Caller should select PassthroughRunner instead — but if we land
            // here, fail loudly rather than silently disabling the sandbox.
            bail!("DangerFullAccess must use PassthroughRunner, not Seatbelt");
        }
        SandboxPolicy::ReadOnly => {
            // No write augmentations. Network stays denied.
        }
        SandboxPolicy::WorkspaceWrite {
            writable_roots,
            network,
        } => {
            // Scratch space is appended here (not in the base profile) so
            // that `ReadOnly` honors its no-writes contract. Build tools,
            // mktemp, and similar utilities need this to function.
            append_scratch_space_allow(&mut s);
            for root in writable_roots {
                append_writable_root(&mut s, root)?;
            }
            // Deny rules come AFTER the allow rules so they win on overlap.
            // Without these, `git config --local`, `python -c 'open(".git/...")'`,
            // and similar argv-opaque write paths would slip the classifier
            // and write to protected metadata inside an otherwise-writable
            // workspace.
            for root in writable_roots {
                append_protected_metadata_deny(&mut s, root)?;
            }
            if *network {
                append_network_allow(&mut s);
            }
        }
    }
    Ok(s)
}

fn append_scratch_space_allow(s: &mut String) {
    s.push_str(
        r#"(allow file-write* (subpath "/tmp"))
(allow file-write* (subpath "/private/tmp"))
(allow file-write* (subpath "/var/tmp"))
(allow file-write* (subpath "/private/var/tmp"))
"#,
    );
}

fn append_writable_root(s: &mut String, root: &Path) -> Result<()> {
    // Use the canonical form so symlinked paths line up with what tools see.
    let canon = root
        .canonicalize()
        .with_context(|| format!("writable root does not exist: {}", root.display()))?;
    let path = canon.to_string_lossy();
    if path.contains('"') {
        bail!("writable root contains a quote: {}", path);
    }
    s.push_str(&format!("(allow file-write* (subpath \"{}\"))\n", path));
    Ok(())
}

/// Appends `(deny file-write* (subpath "<root>/.git"))` (and friends) so the
/// sandbox rejects writes to protected metadata even when our classifier
/// can't see them in argv (e.g. `git config --local`, `python -c '...'`).
fn append_protected_metadata_deny(s: &mut String, root: &Path) -> Result<()> {
    let canon = root
        .canonicalize()
        .with_context(|| format!("writable root does not exist: {}", root.display()))?;
    let path = canon.to_string_lossy();
    if path.contains('"') {
        bail!("writable root contains a quote: {}", path);
    }
    for name in PROTECTED_METADATA_NAMES {
        s.push_str(&format!(
            "(deny file-write* (subpath \"{}/{}\"))\n",
            path, name
        ));
    }
    Ok(())
}

fn append_network_allow(s: &mut String) {
    s.push_str(
        r#"(allow network*)
(allow system-socket)
"#,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::tempdir;

    fn req(command: &str, cwd: PathBuf, policy: SandboxPolicy) -> SandboxRequest {
        SandboxRequest {
            command: command.into(),
            cwd,
            timeout: Duration::from_secs(10),
            policy,
        }
    }

    // ── profile assembly ─────────────────────────────────────────

    #[test]
    fn build_profile_includes_base() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![root.clone()],
            network: false,
        };
        let p = build_profile(&policy).unwrap();
        assert!(p.contains("(version 1)"));
        assert!(p.contains("(deny default)"));
    }

    #[test]
    fn build_profile_appends_writable_root() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![root.clone()],
            network: false,
        };
        let p = build_profile(&policy).unwrap();
        assert!(
            p.contains(&format!(
                "(allow file-write* (subpath \"{}\"))",
                root.display()
            )),
            "profile missing writable-root allow: {p}"
        );
    }

    #[test]
    fn build_profile_denies_protected_metadata_under_writable_root() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![root.clone()],
            network: false,
        };
        let p = build_profile(&policy).unwrap();
        for name in [".git", ".agents", ".nav"] {
            let needle = format!(
                "(deny file-write* (subpath \"{}/{}\"))",
                root.display(),
                name
            );
            assert!(
                p.contains(&needle),
                "profile missing protected-metadata deny ({needle}): {p}"
            );
        }
        // Sanity: deny rules must come AFTER the allow so seatbelt's
        // last-match-wins ordering blocks writes inside `.git`.
        let allow_idx = p
            .find(&format!("(allow file-write* (subpath \"{}\"))", root.display()))
            .expect("allow rule present");
        let deny_idx = p
            .find(&format!(
                "(deny file-write* (subpath \"{}/.git\"))",
                root.display()
            ))
            .expect("deny rule present");
        assert!(allow_idx < deny_idx, "deny must follow allow");
    }

    #[test]
    fn build_profile_appends_network_allow_when_enabled() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network: true,
        };
        let p = build_profile(&policy).unwrap();
        assert!(p.contains("(allow network*)"), "profile missing network allow");
    }

    #[test]
    fn build_profile_omits_network_allow_when_disabled() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network: false,
        };
        let p = build_profile(&policy).unwrap();
        assert!(!p.contains("(allow network*)"));
    }

    #[test]
    fn build_profile_read_only_appends_nothing() {
        // ReadOnly must not append any extra augmentations — and the base
        // policy itself must not contain scratch-space file-write rules
        // (moved to WorkspaceWrite to honor the no-writes contract).
        let p = build_profile(&SandboxPolicy::ReadOnly).unwrap();
        let appended = &p[BASE_POLICY.len()..];
        assert!(
            !appended.contains("file-write*"),
            "ReadOnly should not append file-write: appended = {appended:?}"
        );
        assert!(
            !appended.contains("network"),
            "ReadOnly should not append network: appended = {appended:?}"
        );
    }

    #[test]
    fn build_profile_read_only_does_not_allow_tmp_writes() {
        // Regression: previously the base policy allowed `/tmp` and
        // `/var/tmp` writes, so `--sandbox read-only` silently let
        // `touch /tmp/x` succeed against its documented contract.
        let p = build_profile(&SandboxPolicy::ReadOnly).unwrap();
        for needle in [
            "(allow file-write* (subpath \"/tmp\"))",
            "(allow file-write* (subpath \"/var/tmp\"))",
            "(allow file-write* (subpath \"/private/tmp\"))",
            "(allow file-write* (subpath \"/private/var/tmp\"))",
        ] {
            assert!(
                !p.contains(needle),
                "ReadOnly must not contain scratch-write rule: {needle}"
            );
        }
    }

    #[test]
    fn build_profile_workspace_write_allows_tmp_writes() {
        // WorkspaceWrite still needs scratch space for cargo/mktemp/etc.,
        // so the allow rules are added here (not in base).
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network: false,
        };
        let p = build_profile(&policy).unwrap();
        assert!(p.contains("(allow file-write* (subpath \"/tmp\"))"));
        assert!(p.contains("(allow file-write* (subpath \"/var/tmp\"))"));
    }

    #[test]
    fn build_profile_refuses_danger_full_access() {
        let err = build_profile(&SandboxPolicy::DangerFullAccess).unwrap_err();
        assert!(err.to_string().contains("DangerFullAccess"));
    }

    // ── end-to-end seatbelt invocation (macos only) ──────────────

    /// `sandbox-exec` can't be nested: running these tests inside another
    /// sandbox (e.g. CI agent, codex's review sandbox) fails with
    /// "sandbox_apply: Operation not permitted" before our command runs.
    /// Probe once and skip the e2e assertions when we can't legitimately
    /// observe enforcement.
    #[cfg(target_os = "macos")]
    async fn sandbox_exec_works() -> bool {
        use tokio::process::Command;
        let out = Command::new(SANDBOX_EXEC)
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .output()
            .await;
        matches!(out, Ok(o) if o.status.success())
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn seatbelt_workspace_write_allows_inside_root() {
        if !sandbox_exec_works().await {
            eprintln!("skipping: sandbox-exec unavailable (nested sandbox?)");
            return;
        }
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![cwd.clone()],
            network: false,
        };
        let out = SeatbeltRunner
            .run(req("touch ./hello && ls hello", cwd.clone(), policy))
            .await
            .unwrap();
        assert!(out.stdout.contains("hello"), "stdout was: {}", out.stdout);
        assert_eq!(out.status, Some(0), "stderr: {}", out.stderr);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn seatbelt_workspace_write_denies_outside_root() {
        if !sandbox_exec_works().await {
            eprintln!("skipping: sandbox-exec unavailable (nested sandbox?)");
            return;
        }
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let outside = tempdir().unwrap();
        let outside_path = outside.path().canonicalize().unwrap();
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![cwd.clone()],
            network: false,
        };
        let out = SeatbeltRunner
            .run(req(
                &format!("touch {}/escape", outside_path.display()),
                cwd,
                policy,
            ))
            .await
            .unwrap();
        // Seatbelt refusals surface as nonzero exit + EPERM-ish stderr.
        assert_ne!(out.status, Some(0), "stdout: {}", out.stdout);
        assert!(!outside_path.join("escape").exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn seatbelt_read_only_denies_workspace_write() {
        if !sandbox_exec_works().await {
            eprintln!("skipping: sandbox-exec unavailable (nested sandbox?)");
            return;
        }
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        let out = SeatbeltRunner
            .run(req("touch ./blocked", cwd.clone(), SandboxPolicy::ReadOnly))
            .await
            .unwrap();
        assert_ne!(out.status, Some(0), "stdout: {}", out.stdout);
        assert!(!cwd.join("blocked").exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn seatbelt_read_only_allows_reading() {
        if !sandbox_exec_works().await {
            eprintln!("skipping: sandbox-exec unavailable (nested sandbox?)");
            return;
        }
        let temp = tempdir().unwrap();
        let cwd = temp.path().canonicalize().unwrap();
        std::fs::write(cwd.join("readme.txt"), "hello").unwrap();
        let out = SeatbeltRunner
            .run(req("cat readme.txt", cwd, SandboxPolicy::ReadOnly))
            .await
            .unwrap();
        assert!(out.stdout.contains("hello"));
        assert_eq!(out.status, Some(0));
    }
}
