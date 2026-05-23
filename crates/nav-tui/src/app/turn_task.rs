//! Starts one agent turn in the background.
//!
//! In plain terms: this file takes the prompt the user just submitted,
//! gathers the session history and attachments the agent needs, and launches
//! the real `nav-core` agent work on a Tokio task so the TUI can keep drawing.

use anyhow::{Context, Result};
use nav_core::guardrails::PermissionContext;
use nav_core::{
    AgentEvent, AgentTurnRequest, Catalog, ExtensionCatalog, ModelTransportHandle, ProjectContext,
    SessionBinding, SessionId, SessionStore, TurnControls, UserAttachment, cli::Args,
    rebuild_responses_input, run_agent,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

pub(crate) struct TurnSpawn {
    pub transport: ModelTransportHandle,
    pub args: Args,
    pub cwd: PathBuf,
    pub store: Arc<SessionStore>,
    pub session_id: SessionId,
    pub model_prompt: String,
    pub display_prompt: Option<String>,
    pub attachments: Vec<UserAttachment>,
    pub agent_tx: mpsc::UnboundedSender<AgentEvent>,
    pub skills: Arc<Catalog>,
    pub extensions: Arc<ExtensionCatalog>,
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
        extensions,
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
        let _ = run_agent(
            AgentTurnRequest::new(
                &transport,
                &args,
                &cwd,
                &model_prompt,
                agent_tx.clone(),
                skills.as_ref(),
                permissions,
            )
            .with_display_prompt(display_prompt.as_deref())
            .with_attachments(attachments)
            .with_session(Some(&binding), history_input)
            .with_extensions(Some(extensions.as_ref()))
            .with_context(Some(project.as_ref()))
            .with_controls(controls),
        )
        .await;
    });
    Ok(handle)
}
