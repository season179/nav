use anyhow::Result;
use nav_core::{
    AgentEvent, Catalog, ProjectContext, SessionId, SessionStore,
    build_context_report_with_replay_cwd, cli::{list_models, Args},
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::app::overlay::{Overlay, leave_app_overlay};
use crate::app::resume_picker::build_resume_picker;
use crate::app::terminal::TerminalGuard;
use crate::bottom_pane::{self, ModelPickerEntry};
use crate::custom_terminal::InlineViewportState;
use crate::theme::Theme;
use crate::ChatWidget;

#[allow(clippy::too_many_arguments)]
pub(super) fn push_context_report(
    store: &SessionStore,
    session_id: &SessionId,
    cwd: &Path,
    args: &Args,
    skills: &Catalog,
    project: &ProjectContext,
    include_all: bool,
    chat: &mut ChatWidget,
) {
    match store.load_session(session_id) {
        Ok(events) => {
            let replay_cwd = store
                .session_cwd(session_id)
                .unwrap_or_else(|_| cwd.to_path_buf());
            let report = build_context_report_with_replay_cwd(
                args,
                cwd,
                &replay_cwd,
                &events,
                skills,
                Some(project),
            );
            chat.push_session_notice("context", report.render_text(include_all));
        }
        Err(err) => chat.push_err(err),
    }
}

pub(super) fn resume_session(
    store: &SessionStore,
    query: &str,
) -> Result<(SessionId, Vec<AgentEvent>)> {
    let session_id = store.resolve_session_id(query)?;
    let events = store.load_session(&session_id)?;
    Ok((session_id, events))
}

pub(super) fn try_open_resume_picker(
    store: Arc<SessionStore>,
    exclude_session_id: Option<&str>,
    theme: Theme,
    term: &mut TerminalGuard,
    app_overlay: &mut Option<Overlay>,
    overlay_state: &mut Option<InlineViewportState>,
    chat: &mut ChatWidget,
) {
    if app_overlay.is_some() {
        return;
    }
    let picker = match build_resume_picker(store, exclude_session_id, theme) {
        Ok(picker) => picker,
        Err(err) => return chat.push_err(err),
    };
    match term.terminal.enter_alternate_screen() {
        Ok(state) => {
            *app_overlay = Some(Overlay::Resume(picker));
            *overlay_state = Some(state);
        }
        Err(err) => {
            chat.push_err(anyhow::anyhow!(err).context("Failed to open resume picker"));
        }
    }
}

pub(super) fn try_open_resume_picker_unless_busy(
    turn_running: bool,
    busy_message: &str,
    store: Arc<SessionStore>,
    exclude_session_id: Option<&str>,
    theme: Theme,
    term: &mut TerminalGuard,
    app_overlay: &mut Option<Overlay>,
    overlay_state: &mut Option<InlineViewportState>,
    chat: &mut ChatWidget,
) {
    if turn_running {
        chat.ingest(AgentEvent::Error {
            message: busy_message.to_string(),
        });
        return;
    }
    try_open_resume_picker(
        store,
        exclude_session_id,
        theme,
        term,
        app_overlay,
        overlay_state,
        chat,
    );
}

pub(super) fn dismiss_app_overlay(
    term: &mut TerminalGuard,
    app_overlay: &mut Option<Overlay>,
    overlay_state: &mut Option<InlineViewportState>,
) -> Option<String> {
    let resume_id = app_overlay
        .as_mut()
        .and_then(Overlay::take_resume_selection);
    leave_app_overlay(term, overlay_state);
    *app_overlay = None;
    resume_id
}

pub(super) const NO_PROVIDERS_CONFIGURED: &str =
    "no providers configured — add providers.models to .nav/settings.json";

pub(super) const NO_MODELS_CONFIGURED: &str =
    "no models configured — add entries under providers.models in .nav/settings.json";

pub(super) fn open_model_picker(
    project: &ProjectContext,
    pane: &mut bottom_pane::BottomPane,
    current_model: &str,
    chat: &mut ChatWidget,
) {
    let Some(catalog) = project.settings.providers.as_ref() else {
        chat.push_notice(NO_PROVIDERS_CONFIGURED);
        return;
    };
    let lines = list_models(Some(catalog));
    if lines.is_empty() {
        chat.push_notice(NO_MODELS_CONFIGURED);
        return;
    }
    let entries = lines
        .iter()
        .map(ModelPickerEntry::from_line)
        .collect();
    pane.open_model_picker(entries, Some(current_model));
}

pub(super) fn resolve_tree_root(store: &SessionStore, session_id: &str) -> Result<String> {
    let mut current = session_id.to_string();
    let mut guard = 0u32;
    loop {
        guard += 1;
        if guard > 1024 {
            anyhow::bail!("session tree exceeds 1024 ancestors at {current}");
        }
        match store.session_parent_id(&current)? {
            Some(parent) => current = parent,
            None => return Ok(current),
        }
    }
}

pub(super) fn export_current_session(
    store: &SessionStore,
    session_id: &str,
    cwd: &Path,
    path: Option<PathBuf>,
) -> Result<PathBuf> {
    let display_path = path.unwrap_or_else(|| PathBuf::from(format!("{session_id}.md")));
    let write_path = if display_path.is_absolute() {
        display_path.clone()
    } else {
        cwd.join(&display_path)
    };
    let events = store.load_session(session_id)?;
    let format = nav_core::infer_export_format(Some(&write_path), None);
    let rendered = nav_core::export_events(&events, format)?;
    std::fs::write(&write_path, rendered)?;
    Ok(display_path)
}
