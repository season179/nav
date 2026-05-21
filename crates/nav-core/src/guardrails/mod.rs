//! Guardrails: approval policy, protected reads/writes, command
//! classification, sandbox selection, and path-safety rules.

use std::path::Path;

use crate::agent_loop::{AgentEvent, UserAttachment};
use crate::permissions::approval::ApprovalRequest;
use crate::permissions::protected::is_protected_read;

pub use crate::permissions::classifier::{
    CommandClass, classify_command, classify_pipeline, classify_with_pipeline,
};
pub use crate::permissions::{
    ApprovalReason, AskForApproval, BlockRule, ReviewDecision, SandboxPolicy, SessionAllowlist,
};
#[cfg(target_os = "macos")]
pub use crate::sandbox::SeatbeltRunner;
pub use crate::sandbox::{PassthroughRunner, SandboxRequest, SandboxRunner, select_for_platform};
pub use preflight::{PermissionContext, PreflightOutcome};

/// `tool` field surfaced on approval/block events for protected `@file`
/// attachments. Lets frontends distinguish them from `read_file` calls.
pub const ATTACHMENT_READ_TOOL: &str = "attachment_read";

pub struct ProtectedAttachmentGate {
    pub attachments: Vec<UserAttachment>,
    pub blocked_events: Vec<AgentEvent>,
    pub abort_reason: Option<&'static str>,
}

/// Gate each `File` attachment whose path matches [`is_protected_read`]
/// through the same approval flow the `read_file` tool uses.
///
/// Under `AskForApproval::Never` the gate is short-circuited to `Denied` so a
/// secret cannot ride along when the operator is not around to refuse. The
/// caller still owns event persistence and turn abortion; this function only
/// decides which attachments survive and which safety events should be emitted.
pub async fn gate_protected_attachments(
    attachments: Vec<UserAttachment>,
    permissions: &PermissionContext,
    cwd: &Path,
) -> ProtectedAttachmentGate {
    if !attachments
        .iter()
        .any(|a| matches!(a, UserAttachment::File { path } if is_protected_read(path)))
    {
        return ProtectedAttachmentGate {
            attachments,
            blocked_events: Vec::new(),
            abort_reason: None,
        };
    }

    let auto_denied = preflight::auto_denies_approvals(permissions.policy);
    let mut kept = Vec::with_capacity(attachments.len());
    let mut blocked_events = Vec::new();
    for attach in attachments {
        let UserAttachment::File { path } = &attach else {
            kept.push(attach);
            continue;
        };
        if !is_protected_read(path) {
            kept.push(attach);
            continue;
        }
        let decision = if auto_denied {
            ReviewDecision::Denied
        } else {
            permissions
                .gate
                .request(ApprovalRequest {
                    call_id: String::new(),
                    tool: ATTACHMENT_READ_TOOL.to_string(),
                    command: None,
                    path: Some(path.display().to_string()),
                    cwd: cwd.display().to_string(),
                    reason: ApprovalReason::ProtectedRead.as_str().to_string(),
                })
                .await
        };
        match decision {
            ReviewDecision::Approved | ReviewDecision::ApprovedForSession => kept.push(attach),
            ReviewDecision::Denied => blocked_events.push(AgentEvent::ToolCallBlocked {
                call_id: String::new(),
                tool: ATTACHMENT_READ_TOOL.to_string(),
                reason: if auto_denied {
                    format!(
                        "attachment {} is protected and approval policy is `never`; dropped",
                        path.display()
                    )
                } else {
                    format!("attachment {} denied by user", path.display())
                },
                rule: ApprovalReason::ProtectedRead.as_str().to_string(),
            }),
            ReviewDecision::Abort => {
                return ProtectedAttachmentGate {
                    attachments: Vec::new(),
                    blocked_events,
                    abort_reason: Some("attachment approval abort"),
                };
            }
        }
    }
    ProtectedAttachmentGate {
        attachments: kept,
        blocked_events,
        abort_reason: None,
    }
}

pub mod approval {
    //! Approval request, gate, and durable decision recording.

    pub use crate::permissions::approval::*;
}

pub mod permissions {
    //! Command classification, protected metadata rules, and approval policy.

    pub use crate::permissions::*;
}

pub mod preflight;

pub mod protected {
    //! Protected file and metadata path rules.

    pub use crate::permissions::protected::*;
}

pub mod sandbox {
    //! Platform sandbox adapters.

    pub use crate::sandbox::*;
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use crate::agent_loop::UserAttachment;
    use crate::guardrails::PermissionContext;
    use crate::guardrails::{ATTACHMENT_READ_TOOL, gate_protected_attachments};
    use crate::permissions::approval::AutoGate;
    use crate::permissions::{ApprovalReason, AskForApproval, SandboxPolicy, SessionAllowlist};
    use crate::sandbox::PassthroughRunner;

    fn permissions(policy: AskForApproval, approve: bool) -> PermissionContext {
        let gate = if approve {
            Arc::new(AutoGate::approving())
        } else {
            Arc::new(AutoGate::denying())
        };
        PermissionContext {
            gate,
            policy,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            sandbox: Arc::new(PassthroughRunner),
            session_allowlist: SessionAllowlist::default(),
        }
    }

    #[tokio::test]
    async fn non_protected_attachments_pass_through_without_events() {
        let attachments = vec![UserAttachment::File {
            path: "notes.txt".into(),
        }];
        let outcome = gate_protected_attachments(
            attachments,
            &permissions(AskForApproval::OnRequest, false),
            Path::new("/work"),
        )
        .await;

        assert_eq!(outcome.attachments.len(), 1);
        assert!(outcome.blocked_events.is_empty());
        assert_eq!(outcome.abort_reason, None);
    }

    #[tokio::test]
    async fn never_policy_drops_protected_attachment_and_emits_block() {
        let attachments = vec![UserAttachment::File {
            path: ".env".into(),
        }];
        let outcome = gate_protected_attachments(
            attachments,
            &permissions(AskForApproval::Never, true),
            Path::new("/work"),
        )
        .await;

        assert!(outcome.attachments.is_empty());
        assert_eq!(outcome.abort_reason, None);
        let [crate::agent_loop::AgentEvent::ToolCallBlocked { tool, rule, .. }] =
            outcome.blocked_events.as_slice()
        else {
            panic!("expected one blocked attachment event");
        };
        assert_eq!(tool, ATTACHMENT_READ_TOOL);
        assert_eq!(rule, ApprovalReason::ProtectedRead.as_str());
    }

    #[tokio::test]
    async fn approval_keeps_protected_attachment() {
        let attachments = vec![UserAttachment::File {
            path: ".env.local".into(),
        }];
        let outcome = gate_protected_attachments(
            attachments,
            &permissions(AskForApproval::OnRequest, true),
            Path::new("/work"),
        )
        .await;

        assert_eq!(outcome.attachments.len(), 1);
        assert!(outcome.blocked_events.is_empty());
        assert_eq!(outcome.abort_reason, None);
    }
}
