use anyhow::{Context, Result};
use nav_core::{
    AgentEvent, Catalog, OpenAiTransport, SessionBinding, SessionId, SessionStore, UserAttachment,
    cli::Args, rebuild_responses_input, run_agent,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::input::prepend_pending_skill;

pub(crate) struct TurnSpawn<'a> {
    pub transport: Arc<OpenAiTransport>,
    pub args: Args,
    pub cwd: PathBuf,
    pub store: Arc<SessionStore>,
    pub session_id: SessionId,
    pub raw_prompt: String,
    pub pending_skill: Option<&'a str>,
    pub attachments: Vec<UserAttachment>,
    pub agent_tx: mpsc::UnboundedSender<AgentEvent>,
    pub skills: Arc<Catalog>,
}

pub(crate) fn spawn_turn(request: TurnSpawn<'_>) -> Result<()> {
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
    // Scrollback shows the typed text; the wrapped SKILL.md goes only to the
    // model-facing payload.
    let display_prompt = request.raw_prompt.clone();
    let prompt = prepend_pending_skill(
        request.pending_skill.map(str::to_string),
        &request.raw_prompt,
    );
    let TurnSpawn {
        transport,
        args,
        cwd,
        store,
        session_id,
        attachments,
        agent_tx,
        skills,
        ..
    } = request;

    tokio::spawn(async move {
        let binding = SessionBinding {
            store: store.as_ref(),
            session_id,
        };
        let _ = run_agent(
            transport.as_ref(),
            &args,
            &cwd,
            &prompt,
            Some(&display_prompt),
            attachments,
            agent_tx.clone(),
            Some(&binding),
            history_input,
            skills.as_ref(),
        )
        .await;
    });
    Ok(())
}
