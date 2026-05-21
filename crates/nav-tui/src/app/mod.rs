//! Main TUI application loop.
//!
//! This module wires terminal input, agent events, local slash commands, and
//! rendering together. Child modules hold the lower-level pieces so `run`
//! reads as the high-level lifecycle.

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent};
use nav_core::guardrails::approval::PendingApprovals;
use nav_core::{
    AgentEvent, Catalog, ControlPlane, ExtensionCatalog, HandoffDraft, OpenAiTransport,
    PROVIDER_OPENAI_RESPONSES, PendingInputMode, PendingSkill, PendingSteeringQueue,
    ProjectContext, SessionId, SessionStore, build_handoff_draft,
    cli::{Args, sandbox_policy_from_args},
    git_checkpoint, shorten_home,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

mod permissions;
mod render;
mod session;
mod status_bar;
mod terminal;
mod turn_lifecycle;
mod turn_task;

use crate::ChatWidget;
use crate::bottom_pane::{self, PendingApproval};
use crate::input::{AppEvent, dispatch_submit, handle_scrollback_key, is_ctrl_c};
use crate::theme::Theme;
use permissions::build_tui_permissions;
use render::{TuiStatus, draw_tui};
use session::{
    export_current_session, open_session_picker, push_context_report, resolve_tree_root,
    resume_session,
};
use status_bar::AgentState;
use terminal::{TerminalGuard, enter_tui, install_panic_teardown_hook};
use turn_lifecycle::{
    clear_pending_inputs, pending_draft, pending_input_for_immediate, queue_active_steering,
    remove_active_steering, replace_active_steering, spinner_frame, start_next_follow_up,
    start_pending_turn, turn_is_terminal,
};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    transport: Arc<OpenAiTransport>,
    args: Args,
    cwd: PathBuf,
    store: Arc<SessionStore>,
    mut session_id: SessionId,
    resume_events: Vec<AgentEvent>,
    initial_prompt: Option<String>,
    skills: Arc<Catalog>,
    extensions: Arc<ExtensionCatalog>,
    project: Arc<ProjectContext>,
) -> Result<()> {
    let slash_entries =
        bottom_pane::build_slash_entries_with_extensions(skills.as_ref(), extensions.as_ref());
    let theme = Theme::from_extensions(project.settings.theme.as_deref(), extensions.as_ref());

    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    let mut term = TerminalGuard { terminal };
    enter_tui(term.terminal.backend_mut())?;
    install_panic_teardown_hook();

    // Walk the workspace once at startup so the `@file` popup has something to
    // fuzzy-match against. A re-scan affordance can come later; an idle TUI
    // doesn't need a filesystem watcher to earn its keep.
    let mention_entries = bottom_pane::build_mention_entries(&cwd);

    let branch_summary = project.branch_summary();
    let context_summary = project.context_summary();
    let settings_summary = project.settings_summary(&cwd);

    let mut chat = ChatWidget::with_theme(theme);
    if resume_events.is_empty() {
        chat.push_welcome(
            &args.model,
            cwd.display().to_string(),
            &session_id,
            branch_summary.clone(),
            context_summary.clone(),
            settings_summary.clone(),
        );
    }
    // Rehydrate the visible scrollback at startup. Each submitted turn below
    // rebuilds model-facing history fresh from the session store because
    // `response_body` sends `store: false`.
    for ev in resume_events {
        chat.ingest(ev);
    }
    let mut pane = bottom_pane::BottomPane::with_entries_and_theme(
        slash_entries,
        mention_entries,
        cwd.clone(),
        theme,
    );
    if args.pick_session {
        open_session_picker(&store, &mut pane, Some(&session_id), &mut chat);
    }
    let mut ctrl_c_count = 0u8;
    // A standalone `/<skill>` is a local TUI gesture, not a model turn. Hold
    // its wrapped body here and prepend it onto the next non-slash prompt.
    let mut pending_skill: Option<PendingSkill> = None;
    let mut control = ControlPlane::new();
    let mut active_turn_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut active_steering_queue: Option<PendingSteeringQueue> = None;
    let mut turn_started_at: Option<Instant> = None;
    let mut spinner_tick: u64 = 0;
    let cwd_short = shorten_home(&cwd);
    let branch = project.workspace.branch.clone();
    let dirty = project.workspace.dirty;
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Permission plumbing: pick the gate based on policy. Bypass mode
    // auto-approves through `AutoGate::approving()`; explicit `Never` policy
    // refuses via the in-band short-circuit; otherwise the live TUI gates
    // through `ChannelGate` so the operator can approve interactively.
    let pending_approvals = PendingApprovals::default();
    let sandbox_policy = sandbox_policy_from_args(&args, &cwd);
    let mut permissions = build_tui_permissions(
        &args,
        Arc::clone(&store),
        &session_id,
        agent_tx.clone(),
        pending_approvals.clone(),
        &sandbox_policy,
    );

    if let Some(prompt) = initial_prompt {
        app_tx
            .send(AppEvent::Submit {
                text: prompt,
                display_text: None,
                attachments: Vec::new(),
                mode: PendingInputMode::FollowUp,
                skill: None,
            })
            .ok();
    }

    loop {
        let spinner = spinner_frame(spinner_tick);
        let state = match turn_started_at {
            Some(started) => AgentState::Working {
                elapsed: started.elapsed(),
                spinner,
            },
            None => AgentState::Ready,
        };
        let history_viewport = draw_tui(
            &mut term.terminal,
            &chat,
            &pane,
            TuiStatus {
                model: &args.model,
                cwd_short: &cwd_short,
                branch: branch.as_deref(),
                dirty,
                state,
            },
        )?;

        tokio::select! {
            Some(ev) = agent_rx.recv() => {
                pane.apply_agent_event(&ev);
                if let AgentEvent::PendingInputDequeued { id, .. } = &ev {
                    control.remove_pending(id);
                }
                if turn_is_terminal(&ev) {
                    let active_id = control.active().map(|active| active.id().to_string());
                    if matches!(ev, AgentEvent::TurnAborted { .. })
                        && let Some(id) = active_id.as_deref()
                        && let Ok(abort) = control.abort_turn(id, "turn aborted")
                    {
                        emit_pending_cleared(
                            abort.cleared_steering_ids,
                            store.as_ref(),
                            &session_id,
                            &mut chat,
                            &mut pane,
                        );
                    }
                    turn_started_at = None;
                    active_turn_task = None;
                    active_steering_queue = None;
                    if let Some(id) = active_id
                        && let Ok(settled) = control.finish_turn(&id)
                    {
                        start_next_follow_up(
                            settled.next_follow_up,
                            &mut control,
                            &mut active_turn_task,
                            &mut active_steering_queue,
                            &mut turn_started_at,
                            &transport,
                            &args,
                            &cwd,
                            &store,
                            &session_id,
                            &agent_tx,
                            &skills,
                            &project,
                            &permissions,
                            &mut chat,
                            &mut pane,
                        );
                    }
                }
                if matches!(ev, AgentEvent::UserMessage { .. }) {
                    continue;
                }
                if let AgentEvent::ToolCallApprovalRequest {
                    approval_id,
                    tool,
                    command,
                    path,
                    cwd: req_cwd,
                    reason,
                    ..
                } = &ev
                {
                    pane.enqueue_approval(PendingApproval {
                        approval_id: approval_id.clone(),
                        tool: tool.clone(),
                        command: command.clone(),
                        path: path.clone(),
                        cwd: req_cwd.clone(),
                        reason: reason.clone(),
                    });
                    chat.ingest(ev);
                    continue;
                }
                chat.ingest(ev);
            }
            Some(app) = app_rx.recv() => {
                match app {
                    AppEvent::Quit => break,
                    AppEvent::Clear => {
                        chat = ChatWidget::with_theme(theme);
                        chat.push_welcome(
                            &args.model,
                            cwd.display().to_string(),
                            &session_id,
                            branch_summary.clone(),
                            context_summary.clone(),
                            settings_summary.clone(),
                        );
                        pending_skill = None;
                        clear_pending_inputs(
                            &mut control,
                            &active_steering_queue,
                            store.as_ref(),
                            &session_id,
                            &mut chat,
                            &mut pane,
                        );
                    }
                    AppEvent::AbortTurn => {
                        if let Some(active) = control.active().cloned() {
                            let turn_id = active.id().to_string();
                            let abort = control.abort_turn(&turn_id, "user interrupt").ok();
                            pending_approvals.abort_pending();
                            if let Some(handle) = active_turn_task.take() {
                                handle.abort();
                            }
                            active_steering_queue = None;
                            turn_started_at = None;
                            if let Some(abort) = abort {
                                emit_pending_cleared(
                                    abort.cleared_steering_ids,
                                    store.as_ref(),
                                    &session_id,
                                    &mut chat,
                                    &mut pane,
                                );
                            }
                            emit_local_event(
                                AgentEvent::TurnAborted {
                                    turn_id: turn_id.clone(),
                                    reason: "user interrupt".into(),
                                },
                                store.as_ref(),
                                &session_id,
                                &mut chat,
                                &mut pane,
                            );
                            if let Ok(settled) = control.finish_turn(&turn_id) {
                                start_next_follow_up(
                                    settled.next_follow_up,
                                    &mut control,
                                    &mut active_turn_task,
                                    &mut active_steering_queue,
                                    &mut turn_started_at,
                                    &transport,
                                    &args,
                                    &cwd,
                                    &store,
                                    &session_id,
                                    &agent_tx,
                                    &skills,
                                    &project,
                                    &permissions,
                                    &mut chat,
                                    &mut pane,
                                );
                            }
                        }
                    }
                    AppEvent::EditPending { id, text } => {
                        if let Some(item) = control.edit_pending(&id, text) {
                            replace_active_steering(&active_steering_queue, &item);
                            emit_local_event(
                                AgentEvent::PendingInputEdited {
                                    id: item.id,
                                    text: item.text,
                                    display_text: item.display_text,
                                    attachments: item.attachments,
                                    skill_name: item.skill.map(|skill| skill.name),
                                },
                                store.as_ref(),
                                &session_id,
                                &mut chat,
                                &mut pane,
                            );
                        }
                    }
                    AppEvent::RemovePending { id } => {
                        if let Some(item) = control.remove_pending(&id) {
                            remove_active_steering(&active_steering_queue, &item.id);
                            emit_local_event(
                                AgentEvent::PendingInputRemoved { id: item.id },
                                store.as_ref(),
                                &session_id,
                                &mut chat,
                                &mut pane,
                            );
                        }
                    }
                    AppEvent::ClearPending => {
                        clear_pending_inputs(
                            &mut control,
                            &active_steering_queue,
                            store.as_ref(),
                            &session_id,
                            &mut chat,
                            &mut pane,
                        );
                    }
                    AppEvent::QueueSkill { skill } => {
                        // Replace any previously queued skill; selecting a new
                        // one before sending a prompt should override.
                        pending_skill = Some(skill.clone());
                        chat.scroll_to_bottom();
                        chat.push_user(format!("/{}", skill.name));
                        chat.push_skill(skill.name, "queued for the next prompt");
                    }
                    AppEvent::ListSessions => {
                        match store.list_sessions(None) {
                            Ok(summaries) => {
                                chat.scroll_to_bottom();
                                chat.push_session_list(summaries);
                            }
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::Resume { query: Some(query) } => {
                        if turn_started_at.is_some() {
                            chat.ingest(AgentEvent::Error {
                                message: "cannot resume while a turn is running".to_string(),
                            });
                            continue;
                        }
                        match resume_session(&store, &query) {
                            Ok((resolved, events)) => {
                                session_id = resolved;
                                permissions = build_tui_permissions(
                                    &args,
                                    Arc::clone(&store),
                                    &session_id,
                                    agent_tx.clone(),
                                    pending_approvals.clone(),
                                    &sandbox_policy,
                                );
                                chat = ChatWidget::with_theme(theme);
                                for event in events {
                                    chat.ingest(event);
                                }
                                chat.push_session_notice(
                                    "resume",
                                    format!("Resumed session {session_id}"),
                                );
                            }
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::Resume { query: None } => {
                        if turn_started_at.is_some() {
                            chat.ingest(AgentEvent::Error {
                                message: "cannot resume while a turn is running".to_string(),
                            });
                            continue;
                        }
                        open_session_picker(&store, &mut pane, None, &mut chat);
                    }
                    AppEvent::NameSession { name } => {
                        match store.set_session_name(&session_id, &name) {
                            Ok(()) => chat.push_session_notice(
                                "name",
                                format!("Session name set to \"{}\"", name.trim()),
                            ),
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::Export { path } => {
                        match export_current_session(&store, &session_id, &cwd, path) {
                            Ok(path) => chat.push_session_notice(
                                "export",
                                format!("Wrote transcript to {}", path.display()),
                            ),
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::ShowContext { include_all } => {
                        push_context_report(
                            store.as_ref(),
                            &session_id,
                            &cwd,
                            &args,
                            skills.as_ref(),
                            project.as_ref(),
                            include_all,
                            &mut chat,
                        );
                    }
                    AppEvent::Handoff { goal } => {
                        if turn_started_at.is_some() {
                            chat.ingest(AgentEvent::Error {
                                message: "cannot handoff while a turn is running".to_string(),
                            });
                            continue;
                        }
                        let source_session_id = session_id.clone();
                        match create_handoff_session(
                            store.as_ref(),
                            &source_session_id,
                            &cwd,
                            &args.model,
                            &goal,
                        ) {
                            Ok((new_id, draft)) => {
                                clear_pending_inputs(
                                    &mut control,
                                    &active_steering_queue,
                                    store.as_ref(),
                                    &source_session_id,
                                    &mut chat,
                                    &mut pane,
                                );
                                session_id = new_id;
                                permissions = build_tui_permissions(
                                    &args,
                                    Arc::clone(&store),
                                    &session_id,
                                    agent_tx.clone(),
                                    pending_approvals.clone(),
                                    &sandbox_policy,
                                );
                                control = ControlPlane::new();
                                pending_skill = None;
                                active_steering_queue = None;
                                chat = ChatWidget::with_theme(theme);
                                chat.push_welcome(
                                    &args.model,
                                    cwd.display().to_string(),
                                    &session_id,
                                    branch_summary.clone(),
                                    context_summary.clone(),
                                    settings_summary.clone(),
                                );
                                pane.set_composer_text(&draft.text);
                                chat.push_session_notice(
                                    "handoff",
                                    handoff_notice(&session_id, &draft),
                                );
                            }
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::ForkSession { at } => {
                        if turn_started_at.is_some() {
                            chat.ingest(AgentEvent::Error {
                                message: "cannot fork while a turn is running".to_string(),
                            });
                            continue;
                        }
                        match store.fork_session(&session_id, at, None) {
                            Ok(new_id) => chat.push_session_notice(
                                "fork",
                                format!(
                                    "Forked session {} -> {} at {}",
                                    session_id,
                                    new_id,
                                    at.map(|s| format!("seq={s}"))
                                        .unwrap_or_else(|| "now".to_string()),
                                ),
                            ),
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::RewindSession { at } => {
                        if turn_started_at.is_some() {
                            chat.ingest(AgentEvent::Error {
                                message: "cannot rewind while a turn is running".to_string(),
                            });
                            continue;
                        }
                        let target_seq = if let Some(seq) = at {
                            seq
                        } else {
                            match store.latest_user_message_seq(&session_id) {
                                Ok(Some(seq)) => seq,
                                Ok(None) => {
                                    chat.ingest(AgentEvent::Error {
                                        message: "no user message in this session to rewind to"
                                            .to_string(),
                                    });
                                    continue;
                                }
                                Err(err) => {
                                    chat.push_err(err);
                                    continue;
                                }
                            }
                        };
                        let outcome = match store.rewind_to_user_message(&session_id, target_seq) {
                            Ok(outcome) => outcome,
                            Err(err) => {
                                chat.push_err(err);
                                continue;
                            }
                        };
                        // Rebuild the visible transcript from the trimmed
                        // event log so scrollback matches what the next turn
                        // will replay through `rebuild_responses_input`.
                        let truncated_events = match store.load_session(&session_id) {
                            Ok(events) => events,
                            Err(err) => {
                                chat.push_err(err);
                                continue;
                            }
                        };
                        clear_pending_inputs(
                            &mut control,
                            &active_steering_queue,
                            store.as_ref(),
                            &session_id,
                            &mut chat,
                            &mut pane,
                        );
                        pending_skill = None;
                        chat = ChatWidget::with_theme(theme);
                        chat.push_welcome(
                            &args.model,
                            cwd.display().to_string(),
                            &session_id,
                            branch_summary.clone(),
                            context_summary.clone(),
                            settings_summary.clone(),
                        );
                        for event in truncated_events {
                            chat.ingest(event);
                        }
                        let composer_text = outcome.display_text.unwrap_or(outcome.text);
                        pane.set_composer_text_with_attachments(
                            &composer_text,
                            outcome.attachments,
                        );
                    }
                    AppEvent::ShowTree => match resolve_tree_root(&store, &session_id) {
                        Ok(root_id) => match store.walk_tree(&root_id) {
                            Ok(nodes) => {
                                chat.scroll_to_bottom();
                                chat.push_session_tree(nodes);
                            }
                            Err(err) => chat.push_err(err),
                        },
                        Err(err) => chat.push_err(err),
                    },
                    AppEvent::AddLabel { label } => {
                        match store.add_label(&session_id, &label) {
                            Ok(()) => chat.push_session_notice(
                                "label",
                                format!("Added label \"{}\"", label.trim()),
                            ),
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::RemoveLabel { label } => {
                        match store.remove_label(&session_id, &label) {
                            Ok(()) => chat.push_session_notice(
                                "unlabel",
                                format!("Removed label \"{}\"", label.trim()),
                            ),
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::FindTranscript { query } => {
                        match store.search_transcript(&query, 20, None) {
                            Ok(hits) => {
                                chat.scroll_to_bottom();
                                chat.push_transcript_hits(query, hits);
                            }
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::GitCheckpoint { label } => {
                        run_idle_git_action(
                            "checkpoint",
                            turn_started_at.is_some(),
                            store.as_ref(),
                            &session_id,
                            &mut chat,
                            &mut pane,
                            || git_checkpoint::checkpoint(&cwd, Some(&session_id), label.as_deref()),
                        );
                    }
                    AppEvent::GitStash { label } => {
                        run_idle_git_action(
                            "stash",
                            turn_started_at.is_some(),
                            store.as_ref(),
                            &session_id,
                            &mut chat,
                            &mut pane,
                            || git_checkpoint::stash(&cwd, Some(&session_id), label.as_deref()),
                        );
                    }
                    AppEvent::GitRestore { target } => {
                        run_idle_git_action(
                            "restore",
                            turn_started_at.is_some(),
                            store.as_ref(),
                            &session_id,
                            &mut chat,
                            &mut pane,
                            || git_checkpoint::restore(&cwd, target.as_deref()),
                        );
                    }
                    AppEvent::SlashError { message } => {
                        chat.ingest(AgentEvent::Error { message });
                    }
                    AppEvent::Submit {
                        text,
                        display_text,
                        attachments,
                        mode,
                        skill,
                    } => {
                        let draft = pending_draft(
                            text,
                            display_text,
                            attachments,
                            mode,
                            skill,
                            &mut pending_skill,
                        );
                        if control.active().is_some() {
                            let item = match mode {
                                PendingInputMode::FollowUp => control.enqueue_follow_up(draft),
                                PendingInputMode::Steering => control.enqueue_steering(draft),
                            };
                            if item.mode == PendingInputMode::Steering {
                                queue_active_steering(&active_steering_queue, item.clone());
                            }
                            emit_local_event(
                                AgentEvent::PendingInputQueued {
                                    id: item.id.clone(),
                                    mode: item.mode,
                                    text: item.text.clone(),
                                    display_text: item.display_text.clone(),
                                    attachments: item.attachments.clone(),
                                    skill_name: item.skill.as_ref().map(|skill| skill.name.clone()),
                                },
                                store.as_ref(),
                                &session_id,
                                &mut chat,
                                &mut pane,
                            );
                            continue;
                        }
                        if mode == PendingInputMode::Steering {
                            chat.ingest(AgentEvent::Error {
                                message: "steering can only be queued while a turn is active".into(),
                            });
                            continue;
                        }
                        let item = pending_input_for_immediate(draft);
                        if let Err(err) = start_pending_turn(
                            item,
                            &mut control,
                            &mut active_turn_task,
                            &mut active_steering_queue,
                            &mut turn_started_at,
                            &transport,
                            &args,
                            &cwd,
                            &store,
                            &session_id,
                            &agent_tx,
                            &skills,
                            &project,
                            &permissions,
                            &mut chat,
                        ) {
                            chat.push_err(err);
                        }
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                // 80 ms = ~12 Hz: fast enough that the braille spinner reads as
                // motion, slow enough that an idle TUI doesn't peg a CPU core.
                // The poll(0) below pulls *all* buffered keys per tick so a fast
                // typist never lags a redraw behind their keystrokes.
                spinner_tick = spinner_tick.wrapping_add(1);
                while event::poll(Duration::from_millis(0))? {
                    match event::read()? {
                        CtEvent::Key(key) => {
                            if is_ctrl_c(&key) {
                                if control.active().is_some() {
                                    ctrl_c_count = 0;
                                    app_tx.send(AppEvent::AbortTurn).ok();
                                    continue;
                                }
                                ctrl_c_count += 1;
                                if ctrl_c_count >= 2 {
                                    app_tx.send(AppEvent::Quit).ok();
                                }
                                continue;
                            }
                            ctrl_c_count = 0;
                            if handle_scrollback_key(
                                &mut chat,
                                &key,
                                history_viewport,
                                pane.can_scroll_transcript_with_arrows(),
                            ) {
                                continue;
                            }
                            match pane.handle_key(key) {
                                bottom_pane::ComposerEvent::Submit { text, attachments } => {
                                    dispatch_submit(
                                        text,
                                        attachments,
                                        skills.as_ref(),
                                        extensions.as_ref(),
                                        &app_tx,
                                    );
                                }
                                bottom_pane::ComposerEvent::Nothing
                                | bottom_pane::ComposerEvent::Cancelled => {}
                            }
                        }
                        CtEvent::Paste(text) => {
                            // Bracketed paste was enabled at TUI entry
                            // (see write_tui_enter_sequences); without this
                            // arm the payload would be silently dropped.
                            pane.on_paste(&text);
                        }
                        _ => {}
                    }
                    if let Some((approval_id, decision)) = pane.take_approval_decision() {
                        emit_local_event(
                            AgentEvent::ToolCallApprovalDecision {
                                approval_id: approval_id.clone(),
                                decision,
                            },
                            &store,
                            &session_id,
                            &mut chat,
                            &mut pane,
                        );
                        pending_approvals.respond(&approval_id, decision);
                    }
                    if let Some(session_id) = pane.take_session_selection() {
                        app_tx
                            .send(AppEvent::Resume {
                                query: Some(session_id),
                            })
                            .ok();
                    }
                }
            }
        }
    }
    Ok(())
}

fn emit_local_event(
    event: AgentEvent,
    store: &SessionStore,
    session_id: &SessionId,
    chat: &mut ChatWidget,
    pane: &mut bottom_pane::BottomPane,
) {
    if let Err(err) = store.append_event(session_id, &event) {
        eprintln!("nav-tui: failed to persist local event: {err:#}");
    }
    pane.apply_agent_event(&event);
    chat.ingest(event);
}

fn emit_pending_cleared(
    ids: Vec<String>,
    store: &SessionStore,
    session_id: &SessionId,
    chat: &mut ChatWidget,
    pane: &mut bottom_pane::BottomPane,
) {
    if ids.is_empty() {
        return;
    }
    emit_local_event(
        AgentEvent::PendingInputCleared { ids },
        store,
        session_id,
        chat,
        pane,
    );
}

fn handoff_session_name(goal: &str) -> String {
    let trimmed = goal.trim();
    if trimmed.is_empty() {
        return "handoff".to_string();
    }
    let mut name = format!("handoff: {trimmed}");
    if name.chars().count() > 80 {
        name = name.chars().take(77).collect::<String>();
        name.push_str("...");
    }
    name
}

fn create_handoff_session(
    store: &SessionStore,
    source_session_id: &SessionId,
    cwd: &Path,
    model: &str,
    goal: &str,
) -> Result<(SessionId, HandoffDraft)> {
    let events = store.load_session(source_session_id)?;
    let draft = build_handoff_draft(goal, &events);
    let session_name = handoff_session_name(goal);
    let new_id = store.create_session_named(
        cwd,
        PROVIDER_OPENAI_RESPONSES,
        model,
        None,
        Some(&session_name),
    )?;
    Ok((new_id, draft))
}

fn handoff_notice(session_id: &SessionId, draft: &HandoffDraft) -> String {
    if draft.found_relevant_context {
        return format!(
            "Started fresh session {session_id} with editable handoff draft ({} context item(s))",
            draft.included_entries
        );
    }
    format!(
        "Started fresh session {session_id} with editable handoff draft (no matching prior context)"
    )
}

fn run_idle_git_action(
    name: &str,
    turn_is_active: bool,
    store: &SessionStore,
    session_id: &SessionId,
    chat: &mut ChatWidget,
    pane: &mut bottom_pane::BottomPane,
    run: impl FnOnce() -> Result<git_checkpoint::GitCheckpointOutcome>,
) {
    if turn_is_active {
        chat.ingest(AgentEvent::Error {
            message: format!("cannot {name} while a turn is running"),
        });
        return;
    }
    match run() {
        Ok(outcome) => emit_local_event(outcome.into(), store, session_id, chat, pane),
        Err(err) => chat.push_err(err),
    }
}
