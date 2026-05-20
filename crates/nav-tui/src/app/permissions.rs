use nav_core::guardrails::approval::{ApprovalGate, AutoGate, ChannelGate, PendingApprovals};
use nav_core::guardrails::{
    AskForApproval, PermissionContext, SessionAllowlist, select_for_platform,
};
use nav_core::{AgentEvent, SessionStore, cli::Args};
use std::sync::Arc;
use tokio::sync::mpsc;

pub(super) fn build_tui_permissions(
    args: &Args,
    store: Arc<SessionStore>,
    session_id: &str,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    pending_approvals: PendingApprovals,
    sandbox_policy: &nav_core::SandboxPolicy,
) -> PermissionContext {
    let bypass = args.dangerously_bypass_approvals_and_sandbox;
    let (gate, policy): (Arc<dyn ApprovalGate>, _) = if bypass {
        (
            Arc::new(AutoGate::approving()),
            // Force off `Never` so the gate is consulted instead of being
            // short-circuited to a refusal by `auto_denies_approvals`.
            AskForApproval::OnRequest,
        )
    } else {
        // Attach the session store as a durable sink so the approval request
        // hits the SQLite audit table; the later decision event updates that
        // same row. Rebuilt on TUI resume so approvals are recorded against
        // the active session.
        let channel = ChannelGate::new(pending_approvals, agent_tx)
            .with_sink(Arc::new(store.sink_for(session_id.to_string())));
        (Arc::new(channel), args.approval_policy)
    };
    PermissionContext {
        gate,
        policy,
        sandbox: Arc::from(select_for_platform(sandbox_policy)),
        sandbox_policy: sandbox_policy.clone(),
        // Default empty; populated when the user picks `[a]llow for session`
        // on the approval modal. Shared across spawned turns via Arc.
        session_allowlist: SessionAllowlist::default(),
    }
}
