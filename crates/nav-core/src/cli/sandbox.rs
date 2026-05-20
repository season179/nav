//! Convert human-facing sandbox flags into the guardrail policy used at
//! runtime by the CLI and TUI entry points.

use clap::ValueEnum;
use std::path::Path;

use crate::guardrails::SandboxPolicy;

use super::Args;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

/// Resolve `--sandbox` plus the `--dangerously-bypass-...` flag into the
/// runtime `SandboxPolicy`. Shared between CLI and TUI entry points.
pub fn sandbox_policy_from_args(args: &Args, cwd: &Path) -> SandboxPolicy {
    if args.dangerously_bypass_approvals_and_sandbox {
        return SandboxPolicy::DangerFullAccess;
    }
    match args.sandbox {
        SandboxMode::ReadOnly => SandboxPolicy::ReadOnly,
        SandboxMode::WorkspaceWrite => SandboxPolicy::workspace_write(cwd.to_path_buf()),
        SandboxMode::DangerFullAccess => SandboxPolicy::DangerFullAccess,
    }
}
