//! Resolve a config string into a runtime value.
//!
//! A single function, [`resolve_value`], that interprets a config string with
//! three semantics (evaluated in this order):
//!
//! 1. **Shell command** — if the string starts with `!`, the rest is executed
//!    via `sh -c` and the trimmed stdout is returned. Results are cached for
//!    the process lifetime.
//! 2. **Environment variable** — if `std::env::var(input)` returns a non-empty
//!    value, that value is returned.
//! 3. **Literal** — the original string is returned as-is.
//!
//! The env-over-literal rule means that a literal API key like `sk-…` that
//! happens to match an env-var name will be shadowed by that env var. This is
//! intentional: config files should use the `!command` form for secrets that
//! must not be looked up as env vars.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

/// Process-lifetime cache for shell-command results.
static COMMAND_CACHE: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Resolve a config value using the `!command → env → literal` precedence.
///
/// Returns `Ok(Some(value))` on success, `Ok(None)` when resolution fails in
/// a recoverable way (currently only if an env var is explicitly set to the
/// empty string — treated as "not set"), and `Err` for shell-command failures.
pub fn resolve_value(input: &str) -> Result<Option<String>> {
    if let Some(cmd) = input.strip_prefix('!') {
        return resolve_command(input, cmd);
    }

    // env var wins over literal
    if let Ok(val) = std::env::var(input) {
        if val.is_empty() {
            return Ok(None);
        }
        return Ok(Some(val));
    }

    // literal
    Ok(Some(input.to_owned()))
}

fn resolve_command(cache_key: &str, cmd: &str) -> Result<Option<String>> {
    // Check cache first.
    if let Some(cached) = COMMAND_CACHE.lock().unwrap().get(cache_key).cloned() {
        return Ok(Some(cached));
    }

    let child = std::process::Command::new("sh")
        .args(["-c", cmd])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn shell command: {cmd}"))?;

    // Blocking wait on a background thread, timed out with 10s.
    // Stash the PID so we can kill it on timeout.
    let child_id = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    let output = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result.with_context(|| format!("shell command failed: {cmd}"))?,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let _ = std::process::Command::new("kill")
                .arg(child_id.to_string())
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .spawn();
            bail!("shell command `sh -c '{cmd}'` timed out after 10s");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            bail!("shell command `sh -c '{cmd}'` thread panicked");
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code()
            .map_or("<signal>".to_string(), |c| c.to_string());
        bail!(
            "shell command `sh -c '{cmd}'` exited with status {code}: {stderr}",
        );
    }

    let stdout = String::from_utf8(output.stdout)
        .with_context(|| format!("shell command `sh -c '{cmd}'` produced non-UTF-8 output"))?;

    let trimmed = stdout.trim_end().to_owned();

    // Empty stdout is treated as failure.
    if trimmed.is_empty() {
        bail!(
            "shell command `sh -c '{cmd}'` produced empty output",
        );
    }

    // Store in cache.
    COMMAND_CACHE
        .lock()
        .unwrap()
        .insert(cache_key.to_owned(), trimmed.clone());

    Ok(Some(trimmed))
}

/// Clear the process-lifetime command cache.
///
/// Only needed for targeted cache-bust scenarios; tests should avoid calling
/// this under parallel execution since it clears the global cache for all
/// threads. Prefer using unique commands instead.
#[cfg(test)]
pub fn clear_cache() {
    COMMAND_CACHE.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RAII guard that sets an env var on creation and removes it on drop.
    /// Ensures cleanup even if the test panics.
    struct EnvVarGuard {
        key: String,
    }

    impl EnvVarGuard {
        /// Set `key` to `value` and return a guard that removes it on drop.
        fn new(key: impl Into<String>, value: &str) -> Self {
            let key = key.into();
            // SAFETY: test-only, unique key, no other thread reads this var.
            unsafe { std::env::set_var(&key, value) };
            Self { key }
        }

        /// The env-var name this guard manages.
        fn key(&self) -> &str {
            &self.key
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var(&self.key) };
        }
    }

    // ── Literal resolution ────────────────────────────────────────

    #[test]
    fn literal_string_round_trips() {
        let val = resolve_value("hello-world").unwrap();
        assert_eq!(val, Some("hello-world".to_string()));
    }

    #[test]
    fn literal_api_key_round_trips() {
        let val = resolve_value("sk-abc123def456").unwrap();
        assert_eq!(val, Some("sk-abc123def456".to_string()));
    }

    // ── Env var resolution ────────────────────────────────────────

    #[test]
    fn env_var_resolves_when_set() {
        let guard = EnvVarGuard::new(
            format!("NAV_TEST_RESOLVE_{}", std::process::id()),
            "env-value-123",
        );
        let val = resolve_value(guard.key()).unwrap();
        assert_eq!(val, Some("env-value-123".to_string()));
    }

    #[test]
    fn env_var_empty_returns_none() {
        let guard = EnvVarGuard::new(
            format!("NAV_TEST_RESOLVE_EMPTY_{}", std::process::id()),
            "",
        );
        let val = resolve_value(guard.key()).unwrap();
        assert_eq!(val, None);
    }

    // ── Shell command resolution ──────────────────────────────────
    // Shell-command tests do not call clear_cache() — each uses a different
    // command string so there is no cross-test cache pollution, and calling
    // clear_cache() would race under parallel test execution.

    #[test]
    fn shell_command_returns_stdout() {
        let val = resolve_value("!echo hello").unwrap();
        assert_eq!(val, Some("hello".to_string()));
    }

    #[test]
    fn shell_command_trims_trailing_newline() {
        let val = resolve_value("!echo nav-test-trim").unwrap();
        assert_eq!(val, Some("nav-test-trim".to_string()));
        assert!(!val.unwrap().contains('\n'));
    }

    #[test]
    fn shell_command_failure_returns_error() {
        let err = resolve_value("!false").unwrap_err();
        assert!(err.to_string().contains("exited with status"));
    }

    #[test]
    fn shell_command_empty_stdout_is_error() {
        let err = resolve_value("!true").unwrap_err();
        assert!(err.to_string().contains("empty output"));
    }

    #[test]
    fn shell_command_result_is_cached() {
        let v1 = resolve_value("!date +%s%N").unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let v2 = resolve_value("!date +%s%N").unwrap().unwrap();
        assert_eq!(v1, v2, "second invocation should return the cached result");
    }

    #[test]
    fn shell_command_timeout_is_error() {
        // `sleep 11` exceeds the 10s timeout.
        let err = resolve_value("!sleep 11").unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "expected timeout, got: {err}"
        );
    }

    #[test]
    fn shell_command_stderr_in_error() {
        let err = resolve_value("!echo oops >&2 && false").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("oops"), "stderr should appear in error: {msg}");
    }

    // ── Precedence ────────────────────────────────────────────────

    #[test]
    fn shell_command_takes_precedence_over_env() {
        let key = format!("NAV_TEST_PREC_{}", std::process::id());
        let _guard = EnvVarGuard::new(&key, "from-env");
        let val = resolve_value(&format!("!echo from-cmd-{key}")).unwrap();
        assert_eq!(val, Some(format!("from-cmd-{key}")));
    }

    #[test]
    fn env_var_shadows_literal() {
        let guard = EnvVarGuard::new(
            format!("NAV_TEST_SHADOW_{}", std::process::id()),
            "from-env",
        );
        let val = resolve_value(guard.key()).unwrap();
        assert_eq!(val, Some("from-env".to_string()));
    }
}
