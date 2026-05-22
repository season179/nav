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
    PROVIDER_OPENAI_RESPONSES, PendingInputMode, PendingSkill, ProjectContext, SessionId,
    SessionStore, build_handoff_draft,
    cli::{Args, sandbox_policy_from_args},
    git_checkpoint, shorten_home,
};
use ratatui::backend::CrosstermBackend;
use std::io;

use crate::custom_terminal::Terminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
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
use crate::input::{AppEvent, dispatch_submit, is_ctrl_c};
use crate::theme::Theme;
use crate::widget::parse_rewind_skill_prompt;
use permissions::build_tui_permissions;
use render::{TuiStatus, draw_tui};
use session::{
    export_current_session, open_session_picker, push_context_report, resolve_tree_root,
    resume_session,
};
use status_bar::AgentState;
use terminal::{TerminalGuard, enter_tui, install_panic_teardown_hook};
use turn_lifecycle::{
    ActiveTurnHandle, clear_pending_inputs, pending_draft, pending_input_for_immediate,
    queue_active_steering, remove_active_steering, replace_active_steering, spinner_frame,
    start_next_follow_up, start_pending_turn, turn_is_terminal,
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

    // Enter raw mode + clear stale mouse capture BEFORE constructing the
    // custom Terminal: `Terminal::with_options` issues a CPR query
    // (`ESC[6n`) to discover the cursor row, and the response is only
    // captured in raw mode.
    let mut stdout = io::stdout();
    enter_tui(&mut stdout)?;
    install_panic_teardown_hook();
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::with_options(backend)?;
    let mut term = TerminalGuard { terminal };

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
    let mut active_turn: Option<ActiveTurnHandle> = None;
    let mut spinner_tick: u64 = 0;
    // Latest provider-reported `tokens_input`. For `store: false` transports
    // this is also the current context occupancy, since every turn resends the
    // full history. Updated on `TurnComplete`; pre-first-turn value of `0`
    // renders as `0/200k 0%`.
    let mut last_tokens_input: u64 = 0;
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
    let mut needs_draw = true;
    // Resize coalescing: a drag-resize spits Resize events ~ once per row
    // change. We track the *last applied* width and the *latest pending*
    // width, then run a single reflow per draw cycle. Same-width events
    // become no-ops, so a vertical drag never re-emits the transcript.
    let mut last_reflow_width: u16 = 0;
    let mut pending_reflow_width: Option<u16> = None;

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
        // Per-iteration `TurnComplete` events aren't a reliable "agent done"
        // signal (nav-core fires one after each tool round). The agent task
        // itself ending IS — `run_agent` returns exactly once per user prompt,
        // after the final `TurnComplete`. Reap a finished task here so the
        // status bar flips back to Ready only when work is actually over.
        if active_turn
            .as_ref()
            .is_some_and(ActiveTurnHandle::is_finished)
        {
            needs_draw = true;
            let active_id = control.active().map(|active| active.id().to_string());
            active_turn = None;
            if let Some(id) = active_id
                && let Ok(settled) = control.finish_turn(&id)
            {
                start_next_follow_up(
                    settled.next_follow_up,
                    &mut control,
                    &mut active_turn,
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

        if needs_draw {
            // Single backend-size query per draw cycle: viewport width on
            // startup is zero, so fall back to backend.size() for the very
            // first frame. After that, the viewport tracks the screen.
            let screen_size = term.terminal.size().ok();
            let screen_w = screen_size.map(|s| s.width).unwrap_or(80);
            let screen_h = screen_size.map(|s| s.height).unwrap_or(40);
            let scroll_width = if term.terminal.viewport_area.width > 0 {
                term.terminal.viewport_area.width
            } else {
                screen_w
            };

            // Resize reflow runs at most once per draw cycle (drag-resize
            // events coalesced above). Cap the emitted lines at the visible
            // screen height: anything older is already in the user's
            // scrollback at the previous width and we can't reach into it.
            let reflow_lines = if let Some(new_w) = pending_reflow_width.take() {
                last_reflow_width = new_w;
                chat.reflow_tail_lines(new_w, screen_h as usize)
            } else {
                Vec::new()
            };

            // Flush newly-finalized cells into the terminal's native
            // scrollback above the inline viewport. Use the current
            // viewport width so each cell renders against the same width
            // it would have at draw time.
            let pending = chat.drain_pending(scroll_width);

            // Bracket the scrollback insertion + viewport redraw in a
            // synchronized update so terminals that support it commit
            // both operations atomically — no tearing between the
            // history rows landing above the viewport and the inline
            // viewport repaint. The Begin/End pair is best-effort:
            // terminals without DECSET 2026 support silently ignore it,
            // so a failure here is not actionable.
            use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
            let _ = crossterm::queue!(term.terminal.backend_mut(), BeginSynchronizedUpdate);

            if !reflow_lines.is_empty()
                && let Err(err) = crate::insert_history::insert_history_lines(
                    &mut term.terminal,
                    reflow_lines,
                )
            {
                eprintln!("nav-tui: failed to insert reflowed history rows: {err:#}");
            }

            if !pending.is_empty()
                && let Err(err) = crate::insert_history::insert_history_lines(
                    &mut term.terminal,
                    pending,
                )
            {
                eprintln!("nav-tui: failed to insert pending history rows: {err:#}");
            }

            let spinner = spinner_frame(spinner_tick);
            let state = match active_turn.as_ref() {
                Some(handle) => AgentState::Working {
                    elapsed: handle.elapsed(),
                    spinner,
                },
                None => AgentState::Ready,
            };
            draw_tui(
                &mut term.terminal,
                &chat,
                &pane,
                TuiStatus {
                    model: &args.model,
                    cwd_short: &cwd_short,
                    branch: branch.as_deref(),
                    dirty,
                    state,
                    tokens_input: last_tokens_input,
                    context_window: args.auto_compact_token_limit,
                },
                screen_w,
                screen_h,
            )?;

            // Close the synchronized update; pair this with the Begin
            // above. Use execute! so the terminal commits the queued
            // bytes immediately. Failing here is benign for the same
            // reason as Begin: unsupported terminals ignore the sequence.
            let _ = crossterm::execute!(term.terminal.backend_mut(), EndSynchronizedUpdate);

            needs_draw = false;
        }

        tokio::select! {
            Some(ev) = agent_rx.recv() => {
                needs_draw = true;
                pane.apply_agent_event(&ev);
                if let AgentEvent::PendingInputDequeued { id, .. } = &ev {
                    control.remove_pending(id);
                }
                if let AgentEvent::TurnComplete { usage } = &ev
                    && usage.tokens_input > 0
                {
                    last_tokens_input = usage.tokens_input;
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
                    active_turn = None;
                    if let Some(id) = active_id
                        && let Ok(settled) = control.finish_turn(&id)
                    {
                        start_next_follow_up(
                            settled.next_follow_up,
                            &mut control,
                            &mut active_turn,
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
                needs_draw = true;
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
                            &active_turn,
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
                            if let Some(handle) = active_turn.take() {
                                handle.abort();
                            }
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
                                    &mut active_turn,
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
                            replace_active_steering(&active_turn, &item);
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
                            remove_active_steering(&active_turn, &item.id);
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
                            &active_turn,
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
                        chat.push_user(format!("/{}", skill.name));
                        chat.push_skill(skill.name, "queued for the next prompt");
                    }
                    AppEvent::ListSessions => {
                        match store.list_sessions(None) {
                            Ok(summaries) => {
                                chat.push_session_list(summaries);
                            }
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::Resume { query: Some(query) } => {
                        if active_turn.is_some() {
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
                        if active_turn.is_some() {
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
                        if active_turn.is_some() {
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
                                    &active_turn,
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
                                active_turn = None;
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
                        if active_turn.is_some() {
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
                        if active_turn.is_some() {
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
                            &active_turn,
                            store.as_ref(),
                            &session_id,
                            &mut chat,
                            &mut pane,
                        );
                        // If the rewound message was wrapped by an inline
                        // /skill invocation, restore both the model-facing
                        // wrapper (so the resubmit carries the same skill
                        // instructions the original turn had) and the visible
                        // request (so the composer shows what the user
                        // typed, not the wrapper). Otherwise the resubmit
                        // would silently lose the skill's context — which
                        // for a skill that supplied tool-use guidance or
                        // domain-specific rules could change the behaviour
                        // of the rerun without any visible signal.
                        let (composer_text, restored_skill) = match parse_rewind_skill_prompt(
                            &outcome.text,
                            outcome.display_text.as_deref(),
                        ) {
                            Some(parsed) => (
                                parsed.request,
                                Some(PendingSkill {
                                    name: parsed.name,
                                    wrapped_body: parsed.wrapped_body,
                                }),
                            ),
                            None => (
                                outcome.display_text.unwrap_or(outcome.text),
                                None,
                            ),
                        };
                        pending_skill = restored_skill;
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
                        pane.set_composer_text_with_attachments(
                            &composer_text,
                            outcome.attachments,
                        );
                    }
                    AppEvent::ShowTree => match resolve_tree_root(&store, &session_id) {
                        Ok(root_id) => match store.walk_tree(&root_id) {
                            Ok(nodes) => {
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
                                chat.push_transcript_hits(query, hits);
                            }
                            Err(err) => chat.push_err(err),
                        }
                    }
                    AppEvent::GitCheckpoint { label } => {
                        run_idle_git_action(
                            "checkpoint",
                            active_turn.is_some(),
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
                            active_turn.is_some(),
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
                            active_turn.is_some(),
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
                                queue_active_steering(&active_turn, item.clone());
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
                            &mut active_turn,
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
                // motion while a turn is active. When idle we still wake up to
                // poll crossterm, but avoid repainting unchanged frames so
                // native terminal text selection is not cleared by redraws.
                // The poll(0) below pulls *all* buffered keys per tick so a
                // fast typist never lags a redraw behind their keystrokes.
                if active_turn.is_some() {
                    spinner_tick = spinner_tick.wrapping_add(1);
                    needs_draw = true;
                }
                while event::poll(Duration::from_millis(0))? {
                    match event::read()? {
                        CtEvent::Key(key) => {
                            needs_draw = true;
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
                            // Scrollback navigation is owned by the terminal
                            // (mouse wheel, PgUp/PgDn) — no in-app scroll keys.
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
                            needs_draw = true;
                            // Bracketed paste was enabled at TUI entry
                            // (see write_tui_enter_sequences); without this
                            // arm the payload would be silently dropped.
                            pane.on_paste(&text);
                        }
                        CtEvent::Resize(new_w, _new_h) => {
                            // Coalesce drag-resize: stash the latest width
                            // and let the draw cycle reflow once. Skip the
                            // bookkeeping entirely if the width hasn't
                            // actually changed — vertical drags fire Resize
                            // every row but don't change wrapping. Previous
                            // scrollback at the old width stays put (the
                            // terminal owns scrollback); a visible seam
                            // appears if the user scrolls past the resize.
                            if new_w != last_reflow_width {
                                pending_reflow_width = Some(new_w);
                            }
                            needs_draw = true;
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
