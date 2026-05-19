use anyhow::{Context, Result};
use nav_core::tools::PermissionContext;
use nav_core::{
    AgentEvent, Catalog, OpenAiTransport, ProjectContext, SessionBinding, SessionId, SessionStore,
    TurnControls, UserAttachment, cli::Args, rebuild_responses_input, run_agent_with_control,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

pub(crate) struct TurnSpawn {
    pub transport: Arc<OpenAiTransport>,
    pub args: Args,
    pub cwd: PathBuf,
    pub store: Arc<SessionStore>,
    pub session_id: SessionId,
    pub model_prompt: String,
    pub display_prompt: Option<String>,
    pub attachments: Vec<UserAttachment>,
    pub agent_tx: mpsc::UnboundedSender<AgentEvent>,
    pub skills: Arc<Catalog>,
    pub project: Arc<ProjectContext>,
    pub permissions: PermissionContext,
    pub controls: TurnControls,
}

pub(crate) fn spawn_turn(request: TurnSpawn) -> Result<tokio::task::JoinHandle<()>> {
    let history_events = request
        .store
        .load_session(&request.session_id)
        .context("failed to load session history")?;
    // Replay resolves stored image attachment paths against the session's
    // original cwd, not the resumed process's, so a resume from a different
    // directory still reattaches images saved during the original session.
    let session_cwd = request
        .store
        .session_cwd(&request.session_id)
        .unwrap_or_else(|_| request.cwd.clone());
    let history_input = Some(rebuild_responses_input(&history_events, &session_cwd));
    let TurnSpawn {
        transport,
        args,
        cwd,
        store,
        session_id,
        model_prompt,
        display_prompt,
        attachments,
        agent_tx,
        skills,
        project,
        permissions,
        controls,
        ..
    } = request;

    let handle = tokio::spawn(async move {
        let binding = SessionBinding {
            store: store.as_ref(),
            session_id,
        };
        let _ = run_agent_with_control(
            transport.as_ref(),
            &args,
            &cwd,
            &model_prompt,
            display_prompt.as_deref(),
            attachments,
            agent_tx.clone(),
            Some(&binding),
            history_input,
            skills.as_ref(),
            Some(project.as_ref()),
            permissions,
            controls,
        )
        .await;
    });
    Ok(handle)
}
