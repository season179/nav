//! Sandbox runner trait + per-platform implementations.
//!
//! Phase 1 ships the `Passthrough` runner everywhere — same behavior as the
//! pre-permissions `tools/shell.rs::bash`. Phase 2 adds a real `Seatbelt`
//! runner on macOS (slice 10). Linux/Windows continue passthrough until
//! Phase 3+.
//!
//! The trait is intentionally minimal: callers pass argv + cwd + policy +
//! timeout; runners return captured stdout/stderr/status or a timeout
//! error.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use crate::agent::AbortSignal;
use crate::permissions::SandboxPolicy;

pub mod passthrough;

#[cfg(target_os = "macos")]
pub mod seatbelt;

pub use passthrough::PassthroughRunner;
#[cfg(target_os = "macos")]
pub use seatbelt::SeatbeltRunner;

/// Materialized result of a sandboxed command.
#[derive(Debug, Clone)]
pub struct SandboxOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: Option<i32>,
    pub status_display: String,
}

/// Inputs to a sandboxed run.
#[derive(Debug, Clone)]
pub struct SandboxRequest {
    pub command: String,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub policy: SandboxPolicy,
    /// Turn-scoped cancellation signal. The runner races this against the
    /// child's wait so a long-running bash command can be killed when the
    /// operator presses the abort key. Default value is a never-tripped
    /// signal, matching the prior "wait until timeout or exit" behavior.
    pub abort: AbortSignal,
}

pub trait SandboxRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        req: SandboxRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SandboxOutput>> + Send + 'a>>;
}

/// Pick the appropriate runner for the current platform and policy.
///
/// In Phase 1 every choice resolves to `PassthroughRunner`. Phase 2 wires up
/// macOS Seatbelt for `ReadOnly` / `WorkspaceWrite`; `DangerFullAccess`
/// stays Passthrough by design.
pub fn select_for_platform(policy: &SandboxPolicy) -> Box<dyn SandboxRunner> {
    match policy {
        SandboxPolicy::DangerFullAccess => Box::new(passthrough::PassthroughRunner),
        #[cfg(target_os = "macos")]
        SandboxPolicy::ReadOnly | SandboxPolicy::WorkspaceWrite { .. } => {
            Box::new(seatbelt::SeatbeltRunner)
        }
        #[cfg(not(target_os = "macos"))]
        SandboxPolicy::ReadOnly | SandboxPolicy::WorkspaceWrite { .. } => {
            // Honest gap: no OS sandbox enforcement on this platform yet.
            // Classifier + protected-path rules still apply.
            Box::new(passthrough::PassthroughRunner)
        }
    }
}

/// Helper exposed for tests and direct callers.
pub fn workspace_root_from(policy: &SandboxPolicy, fallback: &Path) -> PathBuf {
    match policy {
        SandboxPolicy::WorkspaceWrite { writable_roots, .. } => writable_roots
            .first()
            .cloned()
            .unwrap_or_else(|| fallback.to_path_buf()),
        _ => fallback.to_path_buf(),
    }
}
