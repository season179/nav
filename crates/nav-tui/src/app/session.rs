use anyhow::Result;
use nav_core::{
    AgentEvent, Catalog, ProjectContext, SessionId, SessionStore,
    build_context_report_with_replay_cwd, cli::Args,
};
use std::path::{Path, PathBuf};

use crate::{ChatWidget, bottom_pane};

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
            chat.scroll_to_bottom();
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

pub(super) fn open_session_picker(
    store: &SessionStore,
    pane: &mut bottom_pane::BottomPane,
    exclude_session_id: Option<&str>,
    chat: &mut ChatWidget,
) {
    match store.list_sessions(None) {
        Ok(summaries) => {
            let entries = summaries
                .iter()
                .filter(|summary| Some(summary.id.as_str()) != exclude_session_id)
                .map(bottom_pane::SessionPickerEntry::from_summary)
                .collect();
            pane.open_session_picker(entries);
        }
        Err(err) => chat.push_err(err),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn open_session_picker_can_exclude_current_empty_session() {
        let (_dir, store) = {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("nav.db");
            let store = SessionStore::open(Some(path)).unwrap();
            (dir, store)
        };
        let current = store
            .create_session(
                Path::new("/repo"),
                nav_core::PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();
        let other = store
            .create_session(
                Path::new("/repo"),
                nav_core::PROVIDER_OPENAI_RESPONSES,
                "gpt-test",
                None,
            )
            .unwrap();
        let mut pane = bottom_pane::BottomPane::new();
        let mut chat = ChatWidget::new();

        open_session_picker(&store, &mut pane, Some(&current), &mut chat);
        pane.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(pane.take_session_selection(), Some(other));
    }
}
