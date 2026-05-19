//! Permission and execution-safety types.
//!
//! Mirrors the outward shapes of `codex-rs/protocol` so non-Rust frontends and
//! operators familiar with codex find consistent names. Three approval levels
//! and three sandbox shapes; codex's `Granular`/`OnFailure`/`ExternalSandbox`
//! variants are intentionally omitted — single-user CLIs don't need them.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub mod approval;
pub mod bash_parse;
pub mod classifier;
pub mod dangerous;
pub mod external;
pub mod protected;
pub mod safe_commands;

/// In-memory cache of (tool, command-or-path) signatures that the user has
/// pre-approved for the rest of the session via `ApprovedForSession`. The
/// `PermissionContext` clone for each spawned turn shares the same `Arc`,
/// so a decision in turn N is visible in turn N+1.
#[derive(Clone, Default)]
pub struct SessionAllowlist {
    inner: Arc<Mutex<HashSet<String>>>,
}

impl SessionAllowlist {
    /// Mark a `(tool, key)` signature as session-approved. `contains` will
    /// return true for the same signature on subsequent tool calls.
    pub fn allow(&self, key: String) {
        self.inner.lock().expect("poisoned").insert(key);
    }

    /// True if this `(tool, key)` was previously session-approved.
    pub fn contains(&self, key: &str) -> bool {
        self.inner.lock().expect("poisoned").contains(key)
    }
}

/// When to ask the user before running a tool.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum AskForApproval {
    /// Auto-run only known-safe read-only commands; ask for everything else.
    #[serde(rename = "untrusted")]
    #[value(name = "untrusted")]
    UnlessTrusted,
    /// Model decides when to escalate. Default for interactive runs.
    #[default]
    OnRequest,
    /// Never prompt. Approval-requiring calls fail to the model as tool errors.
    Never,
}

/// Filesystem/network sandbox shape for the bash tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SandboxPolicy {
    /// No sandboxing whatsoever. Used with --dangerously-bypass-... .
    DangerFullAccess,
    /// Reads anywhere; writes denied; network denied.
    ReadOnly,
    /// Reads anywhere; writes only under `writable_roots`; network is
    /// controlled by the `network` field. `network: true` (the default)
    /// keeps `cargo`, `npm`, `git fetch`, etc. working out of the box;
    /// `network: false` is enforced on macOS via the Seatbelt profile.
    /// Egress-shaped commands (`curl`, `wget`, `nc`, `ssh`) still
    /// escalate at the classifier level so the operator gets a prompt
    /// before they run, independent of this flag.
    WorkspaceWrite {
        writable_roots: Vec<PathBuf>,
        network: bool,
    },
}

impl SandboxPolicy {
    /// Default WorkspaceWrite policy for a freshly-launched interactive session.
    pub fn workspace_write(cwd: PathBuf) -> Self {
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![cwd],
            network: true,
        }
    }
}

/// User-side response to an approval request.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    /// Approved this one call.
    Approved,
    /// Approve this call and remember the (tool, argv prefix) for the session.
    ApprovedForSession,
    /// Reject this call; report as a tool error to the model.
    Denied,
    /// Stop the agent loop entirely.
    Abort,
}

impl ReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewDecision::Approved => "approved",
            ReviewDecision::ApprovedForSession => "approved_for_session",
            ReviewDecision::Denied => "denied",
            ReviewDecision::Abort => "abort",
        }
    }
}

/// Why a tool call needs approval. Stable identifiers — wire-format consumers
/// can branch on these.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReason {
    /// Matched a non-unbypassable dangerous pattern (e.g. `rm -rf build`).
    DangerousPattern,
    /// Not in the safelist; UnlessTrusted policy requires confirmation.
    NotInSafelist,
    /// Reads a protected file like `.env`.
    ProtectedRead,
    /// Writes to a protected metadata path (caught even inside writable roots).
    ProtectedMetadata,
    /// Bash `cd` or working directory steps outside the workspace.
    ExternalDirectory,
    /// Model asked for confirmation explicitly.
    ModelRequested,
}

impl ApprovalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalReason::DangerousPattern => "dangerous_pattern",
            ApprovalReason::NotInSafelist => "not_in_safelist",
            ApprovalReason::ProtectedRead => "protected_read",
            ApprovalReason::ProtectedMetadata => "protected_metadata",
            ApprovalReason::ExternalDirectory => "external_directory",
            ApprovalReason::ModelRequested => "model_requested",
        }
    }
}

/// Stable identifier for a block — surfaced on `ToolCallBlocked.rule`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BlockRule {
    UnbypassableDangerous,
    ProtectedMetadata,
}

impl BlockRule {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockRule::UnbypassableDangerous => "unbypassable_dangerous",
            BlockRule::ProtectedMetadata => "protected_metadata",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_for_approval_default_is_on_request() {
        assert_eq!(AskForApproval::default(), AskForApproval::OnRequest);
    }

    #[test]
    fn ask_for_approval_serde_uses_codex_names() {
        assert_eq!(
            serde_json::to_string(&AskForApproval::UnlessTrusted).unwrap(),
            "\"untrusted\""
        );
        assert_eq!(
            serde_json::to_string(&AskForApproval::OnRequest).unwrap(),
            "\"on-request\""
        );
        assert_eq!(
            serde_json::to_string(&AskForApproval::Never).unwrap(),
            "\"never\""
        );
    }

    #[test]
    fn sandbox_policy_round_trips() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![PathBuf::from("/ws")],
            network: true,
        };
        let json = serde_json::to_string(&policy).unwrap();
        let back: SandboxPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back, policy);
        assert!(json.contains("\"type\":\"workspace-write\""));
        assert!(json.contains("\"writable_roots\""));
    }

    #[test]
    fn sandbox_policy_read_only_serializes_kebab() {
        let json = serde_json::to_string(&SandboxPolicy::ReadOnly).unwrap();
        assert!(json.contains("\"type\":\"read-only\""), "got {json}");
    }

    #[test]
    fn sandbox_policy_danger_full_access_serializes_kebab() {
        let json = serde_json::to_string(&SandboxPolicy::DangerFullAccess).unwrap();
        assert!(
            json.contains("\"type\":\"danger-full-access\""),
            "got {json}"
        );
    }

    #[test]
    fn review_decision_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ReviewDecision::Approved).unwrap(),
            "\"approved\""
        );
        assert_eq!(
            serde_json::to_string(&ReviewDecision::ApprovedForSession).unwrap(),
            "\"approved_for_session\""
        );
        assert_eq!(
            serde_json::to_string(&ReviewDecision::Denied).unwrap(),
            "\"denied\""
        );
        assert_eq!(
            serde_json::to_string(&ReviewDecision::Abort).unwrap(),
            "\"abort\""
        );
    }

    #[test]
    fn review_decision_as_str_matches_serde() {
        for variant in [
            ReviewDecision::Approved,
            ReviewDecision::ApprovedForSession,
            ReviewDecision::Denied,
            ReviewDecision::Abort,
        ] {
            let serde_form = serde_json::to_value(variant).unwrap();
            assert_eq!(serde_form.as_str().unwrap(), variant.as_str());
        }
    }

    #[test]
    fn approval_reason_strings_are_stable() {
        assert_eq!(
            ApprovalReason::DangerousPattern.as_str(),
            "dangerous_pattern"
        );
        assert_eq!(
            ApprovalReason::ProtectedMetadata.as_str(),
            "protected_metadata"
        );
    }

    #[test]
    fn block_rule_strings_are_stable() {
        assert_eq!(
            BlockRule::UnbypassableDangerous.as_str(),
            "unbypassable_dangerous"
        );
        assert_eq!(BlockRule::ProtectedMetadata.as_str(), "protected_metadata");
    }

    #[test]
    fn workspace_write_helper_sets_cwd_and_network() {
        let p = SandboxPolicy::workspace_write(PathBuf::from("/ws"));
        match p {
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                network,
            } => {
                assert_eq!(writable_roots, vec![PathBuf::from("/ws")]);
                assert!(network);
            }
            _ => panic!("expected WorkspaceWrite"),
        }
    }
}
