//! Main TUI application loop.
//!
//! This module wires terminal input, agent events, local slash commands, and
//! rendering together. Child modules hold the lower-level pieces so `run`
//! reads as the high-level lifecycle.

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyModifiers};
use nav_core::guardrails::PermissionContext;
use nav_core::guardrails::approval::PendingApprovals;
use nav_core::{
    AgentEvent, Catalog, ChatCompletionsTransport, ControlPlane, ExtensionCatalog, HandoffDraft,
    ModelSwapOutcome, ModelTransportHandle, NoticeLevel, PROVIDER_OPENAI_RESPONSES,
    PendingInputMode, PendingSkill, ProjectContext, ResolvedProvider, RetryPolicy, SessionId,
    SessionStore, StartupNotices, WireFormat, build_handoff_draft,
    cli::{Args, list_models, sandbox_policy_from_args},
    git_checkpoint, shorten_home,
};
use ratatui::backend::CrosstermBackend;
use std::io;

use crate::app::overlay::{AppOverlay, Overlay, leave_app_overlay};
use crate::custom_terminal::{InlineViewportState, Terminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

mod inline_region;
mod overlay;
mod permissions;
mod resume_picker;
mod render;
mod session;
mod terminal;
mod turn_lifecycle;
mod turn_task;

use crate::ChatWidget;
use crate::bottom_pane::{
    self, AgentState, INDICATOR_SCREEN_FLOOR, PendingApproval, StatusBarState,
};
use crate::chat::parse_rewind_skill_prompt;
use crate::commands::{AppEvent, ModelMatch, dispatch_submit, is_ctrl_c, match_model_selector};
use crate::theme::Theme;
use permissions::build_tui_permissions;
use render::draw_tui;
use session::{
    dismiss_app_overlay, export_current_session, push_context_report, resolve_tree_root,
    resume_session, try_open_resume_picker, try_open_resume_picker_unless_busy,
};
use terminal::{TerminalGuard, enter_tui, install_panic_teardown_hook};
use turn_lifecycle::{
    ActiveTurnHandle, abort_active_turn, clear_pending_inputs, pending_draft,
    pending_input_for_immediate, queue_active_steering, remove_active_steering,
    replace_active_steering, spinner_frame, start_next_follow_up, start_pending_turn,
    turn_is_terminal,
};

/// `true` when the caller should skip the rest of the current `select!` arm
/// (e.g. replayed `UserMessage` is handled elsewhere).
type AgentEventSkip = bool;

enum AgentDrainOutcome {
    Done,
    ContinueLoop,
}

#[allow(clippy::too_many_arguments)]
fn process_agent_event(
    ev: AgentEvent,
    control: &mut ControlPlane,
    active_turn: &mut Option<ActiveTurnHandle>,
    last_tokens_input: &mut u64,
    last_tokens_output: &mut u64,
    last_tokens_cached: &mut u64,
    _store: &SessionStore,
    _session_id: &SessionId,
    chat: &mut ChatWidget,
    pane: &mut bottom_pane::BottomPane,
) -> AgentEventSkip {
    pane.apply_agent_event(&ev);
    if let AgentEvent::PendingInputDequeued { id, .. } = &ev {
        control.remove_pending(id);
    }
    if let AgentEvent::TurnComplete { usage } = &ev {
        if let Some(handle) = active_turn.as_mut() {
            handle.usage.accumulate(usage);
        }
        if usage.tokens_input > 0 {
            *last_tokens_input = usage.tokens_input;
            *last_tokens_output = usage.tokens_output;
            *last_tokens_cached = usage.tokens_input_cached;
        }
    }
    if matches!(ev, AgentEvent::UserMessage { .. }) {
        return true;
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
        return false;
    }
    chat.ingest(ev);
    false
}

#[allow(clippy::too_many_arguments)]
fn settle_terminal_turn(
    is_abort: bool,
    active_turn: &mut Option<ActiveTurnHandle>,
    control: &mut ControlPlane,
    pending_model_swap: &mut Option<PendingModelSwap>,
    transport: &ModelTransportHandle,
    args: &mut Args,
    cwd: &Path,
    store: &Arc<SessionStore>,
    session_id: &SessionId,
    agent_tx: &mpsc::UnboundedSender<AgentEvent>,
    skills: &Arc<Catalog>,
    extensions: &Arc<ExtensionCatalog>,
    project: &Arc<ProjectContext>,
    permissions: &PermissionContext,
    chat: &mut ChatWidget,
    pane: &mut bottom_pane::BottomPane,
) {
    let active_id = control.active().map(|active| active.id().to_string());
    if is_abort
        && let Some(id) = active_id.as_deref()
        && let Ok(abort) = control.abort_turn(id, "turn aborted")
    {
        emit_pending_cleared(
            abort.cleared_steering_ids,
            store.as_ref(),
            session_id,
            chat,
            pane,
        );
    }
    *active_turn = None;
    apply_pending_model_swap(
        pending_model_swap,
        transport,
        args,
        store.as_ref(),
        session_id,
        chat,
    );
    if let Some(id) = active_id
        && let Ok(settled) = control.finish_turn(&id)
    {
        start_next_follow_up(
            settled.next_follow_up,
            control,
            active_turn,
            transport,
            args,
            cwd,
            store,
            session_id,
            agent_tx,
            skills,
            extensions,
            project,
            permissions,
            chat,
            pane,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn reap_finished_turn(
    active_turn: &mut Option<ActiveTurnHandle>,
    control: &mut ControlPlane,
    pending_model_swap: &mut Option<PendingModelSwap>,
    transport: &ModelTransportHandle,
    args: &mut Args,
    cwd: &Path,
    store: &Arc<SessionStore>,
    session_id: &SessionId,
    agent_tx: &mpsc::UnboundedSender<AgentEvent>,
    skills: &Arc<Catalog>,
    extensions: &Arc<ExtensionCatalog>,
    project: &Arc<ProjectContext>,
    permissions: &PermissionContext,
    chat: &mut ChatWidget,
    pane: &mut bottom_pane::BottomPane,
) {
    let active_id = control.active().map(|active| active.id().to_string());
    if let Some(handle) = active_turn.take() {
        chat.push_final_message_separator(handle.elapsed(), handle.usage);
    }
    apply_pending_model_swap(
        pending_model_swap,
        transport,
        args,
        store.as_ref(),
        session_id,
        chat,
    );
    if let Some(id) = active_id
        && let Ok(settled) = control.finish_turn(&id)
    {
        start_next_follow_up(
            settled.next_follow_up,
            control,
            active_turn,
            transport,
            args,
            cwd,
            store,
            session_id,
            agent_tx,
            skills,
            extensions,
            project,
            permissions,
            chat,
            pane,
        );
    }
}

/// Drain queued agent events for a finished task, then reap once the channel is empty.
#[allow(clippy::too_many_arguments)]
fn drain_agent_events_or_reap_finished_turn(
    agent_rx: &mut mpsc::UnboundedReceiver<AgentEvent>,
    active_turn: &mut Option<ActiveTurnHandle>,
    control: &mut ControlPlane,
    pending_model_swap: &mut Option<PendingModelSwap>,
    transport: &ModelTransportHandle,
    args: &mut Args,
    cwd: &Path,
    store: &Arc<SessionStore>,
    session_id: &SessionId,
    agent_tx: &mpsc::UnboundedSender<AgentEvent>,
    skills: &Arc<Catalog>,
    extensions: &Arc<ExtensionCatalog>,
    project: &Arc<ProjectContext>,
    permissions: &PermissionContext,
    last_tokens_input: &mut u64,
    last_tokens_output: &mut u64,
    last_tokens_cached: &mut u64,
    chat: &mut ChatWidget,
    pane: &mut bottom_pane::BottomPane,
) -> AgentDrainOutcome {
    loop {
        match agent_rx.try_recv() {
            Ok(ev) => {
                let terminal = turn_is_terminal(&ev);
                let is_abort = matches!(&ev, AgentEvent::TurnAborted { .. });
                if process_agent_event(
                    ev,
                    control,
                    active_turn,
                    last_tokens_input,
                    last_tokens_output,
                    last_tokens_cached,
                    store.as_ref(),
                    session_id,
                    chat,
                    pane,
                ) {
                    return AgentDrainOutcome::ContinueLoop;
                }
                if terminal {
                    settle_terminal_turn(
                        is_abort,
                        active_turn,
                        control,
                        pending_model_swap,
                        transport,
                        args,
                        cwd,
                        store,
                        session_id,
                        agent_tx,
                        skills,
                        extensions,
                        project,
                        permissions,
                        chat,
                        pane,
                    );
                    return AgentDrainOutcome::Done;
                }
            }
            Err(TryRecvError::Empty) => {
                reap_finished_turn(
                    active_turn,
                    control,
                    pending_model_swap,
                    transport,
                    args,
                    cwd,
                    store,
                    session_id,
                    agent_tx,
                    skills,
                    extensions,
                    project,
                    permissions,
                    chat,
                    pane,
                );
                return AgentDrainOutcome::Done;
            }
            Err(TryRecvError::Disconnected) => {
                eprintln!("nav-tui: agent event channel disconnected");
                reap_finished_turn(
                    active_turn,
                    control,
                    pending_model_swap,
                    transport,
                    args,
                    cwd,
                    store,
                    session_id,
                    agent_tx,
                    skills,
                    extensions,
                    project,
                    permissions,
                    chat,
                    pane,
                );
                return AgentDrainOutcome::Done;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    transport: ModelTransportHandle,
    mut args: Args,
    cwd: PathBuf,
    store: Arc<SessionStore>,
    mut session_id: SessionId,
    resume_events: Vec<AgentEvent>,
    initial_prompt: Option<String>,
    skills: Arc<Catalog>,
    extensions: Arc<ExtensionCatalog>,
    project: Arc<ProjectContext>,
    startup_notices: StartupNotices,
) -> Result<()> {
    let slash_entries =
        bottom_pane::build_slash_entries_with_extensions(skills.as_ref(), extensions.as_ref());
    let skill_entries = bottom_pane::build_skill_entries(skills.as_ref());

    // Enter raw mode + clear stale mouse capture BEFORE constructing the
    // custom Terminal: `Terminal::with_options` issues a CPR query
    // (`ESC[6n`) to discover the cursor row, and the response is only
    // captured in raw mode.
    let mut stdout = io::stdout();
    enter_tui(&mut stdout)?;
    #[cfg(unix)]
    crate::terminal_palette::probe_default_colors_at_startup();
    let theme = Theme::from_extensions(project.settings.theme.as_deref(), extensions.as_ref());
    install_panic_teardown_hook();
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::with_options(backend)?;
    let mut term = TerminalGuard { terminal };

    // Walk the workspace once at startup so the `@file` popup has something to
    // fuzzy-match against. A re-scan affordance can come later; an idle TUI
    // doesn't need a filesystem watcher to earn its keep.
    let mention_entries = bottom_pane::build_mention_entries(&cwd);

    let mut chat = ChatWidget::with_theme(theme);
    // Replay startup-time warnings as styled cells so skill/extension
    // discovery messages live in scrollback like the rest of the
    // transcript, instead of stderr leaking above the inline viewport.
    for notice in startup_notices.iter() {
        match notice.level {
            NoticeLevel::Warning => chat.push_warning(notice.message.clone()),
            NoticeLevel::Error => chat.push_error_notice(notice.message.clone()),
        }
    }
    // Rehydrate the visible scrollback at startup. Each submitted turn below
    // rebuilds model-facing history fresh from the session store because
    // `response_body` sends `store: false`.
    for ev in resume_events {
        chat.ingest(ev);
    }
    let mut pane = bottom_pane::BottomPane::with_entries_and_skill(
        slash_entries,
        mention_entries,
        skill_entries,
        cwd.clone(),
        theme,
    );
    let mut ctrl_c_count = 0u8;
    // A standalone `/<skill>` is a local TUI gesture, not a model turn. Hold
    // its wrapped body here and prepend it onto the next non-slash prompt.
    let mut pending_skill: Option<PendingSkill> = None;
    let mut control = ControlPlane::new();
    let mut active_turn: Option<ActiveTurnHandle> = None;
    let mut pending_model_swap: Option<PendingModelSwap> = None;
    let mut spinner_tick: u64 = 0;
    let mut app_overlay: Option<Overlay> = None;
    let mut overlay_state: Option<InlineViewportState> = None;
    if args.pick_session {
        try_open_resume_picker(
            Arc::clone(&store),
            Some(&session_id),
            theme,
            &mut term,
            &mut app_overlay,
            &mut overlay_state,
            &mut chat,
        );
    }
    // Latest provider-reported token usage for the status bar.
    // Updated on `TurnComplete`; pre-first-turn value of `0` hides the gauge.
    let mut last_tokens_input: u64 = 0;
    let mut last_tokens_output: u64 = 0;
    let mut last_tokens_cached: u64 = 0;
    let mut input_tick = tokio::time::interval(Duration::from_millis(80));
    input_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let cwd_short = shorten_home(&cwd);
    let branch = project.workspace.branch.clone();
    let dirty = project.workspace.dirty;
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Permission plumbing: pick the gate based on policy. Bypass mode
    // auto-approves through `AutoGate::approving()`; explicit `Never` policy
    // refuses via the in-band short-circuit; otherwise the live TUI gates
    // through `ChannelGate` so the operator can approve interactively.
    let mut pending_approvals = PendingApprovals::default();
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
    let mut quit_requested = false;

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
            match agent_rx.try_recv() {
                Ok(ev) => {
                    let terminal = turn_is_terminal(&ev);
                    let is_abort = matches!(&ev, AgentEvent::TurnAborted { .. });
                    if process_agent_event(
                        ev,
                        &mut control,
                        &mut active_turn,
                        &mut last_tokens_input,
                        &mut last_tokens_output,
                        &mut last_tokens_cached,
                        store.as_ref(),
                        &session_id,
                        &mut chat,
                        &mut pane,
                    ) {
                        continue;
                    }
                    if terminal {
                        settle_terminal_turn(
                            is_abort,
                            &mut active_turn,
                            &mut control,
                            &mut pending_model_swap,
                            &transport,
                            &mut args,
                            &cwd,
                            &store,
                            &session_id,
                            &agent_tx,
                            &skills,
                            &extensions,
                            &project,
                            &permissions,
                            &mut chat,
                            &mut pane,
                        );
                    } else if matches!(
                        drain_agent_events_or_reap_finished_turn(
                            &mut agent_rx,
                            &mut active_turn,
                            &mut control,
                            &mut pending_model_swap,
                            &transport,
                            &mut args,
                            &cwd,
                            &store,
                            &session_id,
                            &agent_tx,
                            &skills,
                            &extensions,
                            &project,
                            &permissions,
                            &mut last_tokens_input,
                            &mut last_tokens_output,
                            &mut last_tokens_cached,
                            &mut chat,
                            &mut pane,
                        ),
                        AgentDrainOutcome::ContinueLoop
                    ) {
                        continue;
                    }
                }
                Err(TryRecvError::Empty) => reap_finished_turn(
                    &mut active_turn,
                    &mut control,
                    &mut pending_model_swap,
                    &transport,
                    &mut args,
                    &cwd,
                    &store,
                    &session_id,
                    &agent_tx,
                    &skills,
                    &extensions,
                    &project,
                    &permissions,
                    &mut chat,
                    &mut pane,
                ),
                Err(TryRecvError::Disconnected) => {
                    eprintln!("nav-tui: agent event channel disconnected");
                    reap_finished_turn(
                        &mut active_turn,
                        &mut control,
                        &mut pending_model_swap,
                        &transport,
                        &mut args,
                        &cwd,
                        &store,
                        &session_id,
                        &agent_tx,
                        &skills,
                        &extensions,
                        &project,
                        &permissions,
                        &mut chat,
                        &mut pane,
                    );
                }
            }
        }

        if needs_draw {
            // Single backend-size query per draw cycle: viewport width on
            // startup is zero, so fall back to backend.size() for the very
            // first frame. After that, the viewport tracks the screen.
            let screen_size = term.terminal.size().ok();
            let screen_w = screen_size.map(|s| s.width).unwrap_or(80);
            let screen_h = screen_size.map(|s| s.height).unwrap_or(40);

            let spinner = spinner_frame(spinner_tick);
            let state = match active_turn.as_ref() {
                Some(handle) => AgentState::Working {
                    elapsed: handle.elapsed(),
                    spinner,
                    tick: spinner_tick,
                },
                None => AgentState::Ready,
            };
            // Dedicated indicator row only when the agent is actually
            // working AND there's vertical room. Below the floor the
            // spinner stays inline in the status bar.
            let show_indicator =
                matches!(state, AgentState::Working { .. }) && screen_h >= INDICATOR_SCREEN_FLOOR;
            pane.update_status(StatusBarState {
                model: args.model.clone(),
                cwd_short: cwd_short.clone(),
                branch: branch.clone(),
                dirty,
                agent_state: state,
                tokens_input: last_tokens_input,
                tokens_output: last_tokens_output,
                tokens_cached: last_tokens_cached,
                context_window: args.auto_compact_token_limit,
                show_indicator,
            });
            if let Some(overlay) = app_overlay.as_mut() {
                let overlay_width = screen_w.saturating_sub(2).max(1);
                overlay.prepare(&chat, overlay_width, spinner_tick);
            }
            let overlay: Option<&dyn AppOverlay> =
                app_overlay.as_ref().map(|o| o as &dyn AppOverlay);
            draw_tui(
                &mut term.terminal,
                &chat,
                &pane,
                screen_w,
                screen_h,
                overlay,
            )?;

            if app_overlay.is_some() {
                needs_draw = false;
                continue;
            }

            // Bracket the inline viewport redraw + scrollback insertion in a
            // synchronized update so terminals that support it commit
            // both operations atomically — no tearing between the
            // inline viewport repaint and the history rows landing
            // above it. The Begin/End pair is best-effort: terminals
            // without DECSET 2026 support silently ignore it, so a
            // failure here is not actionable.
            use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
            let _ = crossterm::queue!(term.terminal.backend_mut(), BeginSynchronizedUpdate);

            let scroll_width = if term.terminal.viewport_area.width > 0 {
                term.terminal.viewport_area.width
            } else {
                screen_w
            };
            // Pull newly-finalized cells out of the chat ahead of the
            // viewport resize so the resize sees the in-flight cells
            // (streaming, tool placeholders) only — and the final flush
            // then clears the inline frame.
            let pending = chat.drain_pending(scroll_width);

            // Flush finalized rows into native scrollback AFTER the resize.
            // When the viewport just shrank (e.g. a streaming cell
            // finalized), `insert_history_lines` finds slide room equal to
            // the shrinkage and slides the smaller viewport DOWN — the
            // composer re-anchors at the screen floor without a blank
            // band above. This mirrors codex's draw flow (resize first,
            // then flush_pending_history_lines).
            if !pending.is_empty()
                && let Err(err) = crate::insert_history::insert_history_lines(
                    &mut term.terminal,
                    pending,
                    scroll_width,
                )
            {
                eprintln!("nav-tui: failed to insert pending history rows: {err:#}");
            }

            // Collapsed exploration cells produce fewer scrollback rows
            // than the inline placeholders they replace, so the normal
            // slide in insert_history_lines may not reach the screen floor.
            // Clamp the viewport down to close the gap.
            if let Err(err) = crate::insert_history::clamp_viewport_to_floor(&mut term.terminal) {
                eprintln!("nav-tui: failed to clamp viewport to floor: {err:#}");
            }

            // Close the synchronized update; pair this with the Begin
            // above. Use execute! so the terminal commits the queued
            // bytes immediately. Failing here is benign for the same
            // reason as Begin: unsupported terminals ignore the sequence.
            let _ = crossterm::execute!(term.terminal.backend_mut(), EndSynchronizedUpdate);

            needs_draw = false;
        }

        if quit_requested {
            // Drop the alt-screen on the way out so the user's terminal
            // returns to whatever inline state it had before the overlay.
            // The `app_overlay = None` reset that would normally accompany
            // this is elided — `break` exits the loop immediately.
            leave_app_overlay(&mut term, &mut overlay_state);
            break;
        }

        tokio::select! {
            Some(ev) = agent_rx.recv() => {
                needs_draw = true;
                let terminal = turn_is_terminal(&ev);
                let is_abort = matches!(&ev, AgentEvent::TurnAborted { .. });
                if process_agent_event(
                    ev,
                    &mut control,
                    &mut active_turn,
                    &mut last_tokens_input,
                    &mut last_tokens_output,
                    &mut last_tokens_cached,
                    store.as_ref(),
                    &session_id,
                    &mut chat,
                    &mut pane,
                ) {
                    continue;
                }
                if terminal {
                    settle_terminal_turn(
                        is_abort,
                        &mut active_turn,
                        &mut control,
                        &mut pending_model_swap,
                        &transport,
                        &mut args,
                        &cwd,
                        &store,
                        &session_id,
                        &agent_tx,
                        &skills,
                        &extensions,
                        &project,
                        &permissions,
                        &mut chat,
                        &mut pane,
                    );
                } else if active_turn
                    .as_ref()
                    .is_some_and(ActiveTurnHandle::is_finished)
                    && matches!(
                        drain_agent_events_or_reap_finished_turn(
                            &mut agent_rx,
                            &mut active_turn,
                            &mut control,
                            &mut pending_model_swap,
                            &transport,
                            &mut args,
                            &cwd,
                            &store,
                            &session_id,
                            &agent_tx,
                            &skills,
                            &extensions,
                            &project,
                            &permissions,
                            &mut last_tokens_input,
                            &mut last_tokens_output,
                            &mut last_tokens_cached,
                            &mut chat,
                            &mut pane,
                        ),
                        AgentDrainOutcome::ContinueLoop
                    )
                {
                    continue;
                }
            }
            Some(app) = app_rx.recv() => {
                needs_draw = true;
                match app {
                    AppEvent::Quit => {
                        // Promote any in-flight exploration summary into a
                        // finalized cell so the next iteration's draw block
                        // (already armed by `needs_draw = true` above) writes
                        // it to scrollback. The actual exit happens after
                        // that draw via the `quit_requested` check at the
                        // top of the loop.
                        chat.flush_pending_for_shutdown();
                        quit_requested = true;
                    }
                    AppEvent::Clear => {
                        chat = ChatWidget::with_theme(theme);
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
                        abort_active_turn(
                            &mut control,
                            &mut active_turn,
                            &mut pending_approvals,
                            &transport,
                            &args,
                            &cwd,
                            &store,
                            &session_id,
                            &agent_tx,
                            &skills,
                            &extensions,
                            &project,
                            &permissions,
                            &mut chat,
                            &mut pane,
                        );
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
                        try_open_resume_picker_unless_busy(
                            active_turn.is_some(),
                            "cannot open session picker while a turn is running",
                            Arc::clone(&store),
                            None,
                            theme,
                            &mut term,
                            &mut app_overlay,
                            &mut overlay_state,
                            &mut chat,
                        );
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
                        try_open_resume_picker_unless_busy(
                            active_turn.is_some(),
                            "cannot resume while a turn is running",
                            Arc::clone(&store),
                            None,
                            theme,
                            &mut term,
                            &mut app_overlay,
                            &mut overlay_state,
                            &mut chat,
                        );
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
                    AppEvent::ListModels => {
                        let lines = list_models(project.settings.providers.as_ref());
                        chat.push_model_list(
                            lines,
                            args.model.clone(),
                            project.settings.default_model.clone(),
                        );
                    }
                    AppEvent::SetModel { selector } => {
                        let catalog = project.settings.providers.as_ref();
                        let Some(catalog) = catalog else {
                            chat.push_model_set(
                                "no providers configured — add providers.models to .nav/settings.json",
                            );
                            continue;
                        };
                        match match_model_selector(&selector, catalog) {
                            ModelMatch::Exact(sel) | ModelMatch::BareUnique(sel) => {
                                match resolve_model_swap(&sel, project.as_ref()) {
                                    Ok(swap) if active_turn.is_some() => {
                                        pending_model_swap = Some(swap);
                                        chat.push_model_set(format!(
                                            "Queued model swap to \"{sel}\" after the current turn."
                                        ));
                                    }
                                    Ok(swap) => apply_model_swap(
                                        swap,
                                        &transport,
                                        &mut args,
                                        store.as_ref(),
                                        &session_id,
                                        &mut chat,
                                    ),
                                    Err(err) => chat.push_err(err),
                                }
                            }
                            ModelMatch::Ambiguous(sels) => {
                                let list = sels.join("\n  ");
                                chat.push_model_set(format!(
                                    "\"{selector}\" is ambiguous — matches:\n  {list}\n\
                                    Use a qualified <provider>/<model> selector."
                                ));
                            }
                            ModelMatch::NotFound => {
                                chat.push_model_set(format!(
                                    "No model matches \"{selector}\". Run /model to list."
                                ));
                            }
                        }
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
                            &extensions,
                            &project,
                            &permissions,
                            &mut chat,
                        ) {
                            chat.push_err(err);
                        }
                    }
                }
            }
            _ = input_tick.tick() => {
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
                // Drive the streaming chunking policy on the same tick. The
                // policy advances `visible_stable_lines` by 1 in smooth mode
                // and in bulk during catch-up; without this call the
                // streaming cell paints the entire stable region the moment
                // it lands, defeating the smoothing layer. Outside an active
                // turn there's no streaming cell, so this is a cheap no-op.
                if chat.on_commit_tick() {
                    needs_draw = true;
                }
                while event::poll(Duration::from_millis(0))? {
                    match event::read()? {
                        CtEvent::Key(key) => {
                            needs_draw = true;
                            if let Some(overlay) = app_overlay.as_mut() {
                                let overlay_result = overlay.handle_key(key);
                                if overlay.is_complete() {
                                    if let Some(id) = dismiss_app_overlay(
                                        &mut term,
                                        &mut app_overlay,
                                        &mut overlay_state,
                                    ) {
                                        app_tx
                                            .send(AppEvent::Resume {
                                                query: Some(id),
                                            })
                                            .ok();
                                    }
                                    continue;
                                }
                                if matches!(overlay_result, bottom_pane::InputResult::Handled) {
                                    continue;
                                }
                            } else if is_ctrl_t(&key) {
                                match term.terminal.enter_alternate_screen() {
                                    Ok(state) => {
                                        app_overlay = Some(Overlay::transcript());
                                        overlay_state = Some(state);
                                    }
                                    Err(err) => {
                                        chat.push_err(anyhow::anyhow!(err)
                                            .context("Failed to open overlay"));
                                    }
                                }
                                continue;
                            }

                            if is_ctrl_c(&key) {
                                if pane.handle_ctrl_c() {
                                    ctrl_c_count = 0;
                                    continue;
                                }
                                if control.active().is_some() {
                                    ctrl_c_count = 0;
                                    abort_active_turn(
                                        &mut control,
                                        &mut active_turn,
                                        &mut pending_approvals,
                                        &transport,
                                        &args,
                                        &cwd,
                                        &store,
                                        &session_id,
                                        &agent_tx,
                                        &skills,
                                        &extensions,
                                        &project,
                                        &permissions,
                                        &mut chat,
                                        &mut pane,
                                    );
                                    continue;
                                }
                                ctrl_c_count += 1;
                                if ctrl_c_count >= 2 {
                                    app_tx.send(AppEvent::Quit).ok();
                                }
                                continue;
                            }
                            ctrl_c_count = 0;
                            // Ctrl+L — force a full redraw (readline convention).
                            // Invalidate the diff base so flush() repaints every
                            // cell, not just ones that differ from last frame.
                            if is_ctrl_l(&key) {
                                term.terminal.invalidate_previous_buffer();
                                needs_draw = true;
                                continue;
                            }
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
                        CtEvent::Resize(_new_w, _new_h) => {
                            // The terminal handles re-wrapping its own
                            // scrollback at the new width; nav doesn't
                            // re-emit cells. A previous version did, which
                            // produced a duplicate transcript: re-emitted
                            // rows landed in scrollback _above_ the
                            // identical old-width copies the terminal had
                            // already kept. The visible seam at the resize
                            // point (old rows hard-wrapped or padded by the
                            // terminal) is accepted by design — see the
                            // discussion in CLAUDE.md.
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
                }
                if pane.promote_pending_approval_if_idle() {
                    needs_draw = true;
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

fn is_ctrl_t(key: &crossterm::event::KeyEvent) -> bool {
    key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Check whether `key` is a Ctrl+L press (no Alt, to avoid colliding
/// with Ctrl+Alt+L on international layouts). Ignores Release events so
/// the handler fires exactly once per physical keypress.
fn is_ctrl_l(key: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::KeyEventKind;
    key.kind == KeyEventKind::Press
        && key.code == KeyCode::Char('l')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
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

#[derive(Clone)]
struct PendingModelSwap {
    selector: String,
    resolved: ResolvedProvider,
}

fn resolve_model_swap(selector: &str, project: &ProjectContext) -> Result<PendingModelSwap> {
    let resolved = nav_core::model::resolve_provider(Some(selector), &project.settings)?;
    Ok(PendingModelSwap {
        selector: selector.to_string(),
        resolved,
    })
}

fn apply_pending_model_swap(
    pending: &mut Option<PendingModelSwap>,
    transport: &ModelTransportHandle,
    args: &mut Args,
    store: &SessionStore,
    session_id: &SessionId,
    chat: &mut ChatWidget,
) {
    let Some(swap) = pending.take() else {
        return;
    };
    apply_model_swap(swap, transport, args, store, session_id, chat);
}

fn apply_model_swap(
    swap: PendingModelSwap,
    transport: &ModelTransportHandle,
    args: &mut Args,
    store: &SessionStore,
    session_id: &SessionId,
    chat: &mut ChatWidget,
) {
    match swap_active_transport(&swap, transport, args, store, session_id) {
        Ok(outcome) => {
            args.model = swap.selector.clone();
            chat.push_model_set(model_swap_message(&swap.selector, outcome.from, outcome.to));
        }
        Err(err) => chat.push_err(err),
    }
}

fn swap_active_transport(
    swap: &PendingModelSwap,
    transport: &ModelTransportHandle,
    args: &Args,
    store: &SessionStore,
    session_id: &SessionId,
) -> Result<ModelSwapOutcome> {
    let next = build_chat_completions_transport(swap, args)?;
    store.set_session_model(session_id, &swap.selector)?;
    transport.swap_to(next)
}

fn build_chat_completions_transport(
    swap: &PendingModelSwap,
    args: &Args,
) -> Result<ChatCompletionsTransport> {
    ChatCompletionsTransport::with_default_client(
        swap.resolved.clone(),
        Duration::from_secs(args.idle_timeout_secs),
        RetryPolicy::default(),
    )
}

fn model_swap_message(selector: &str, from: WireFormat, to: WireFormat) -> String {
    if from == to {
        return format!("Switched model to \"{selector}\".");
    }
    format!(
        "Switched model to \"{selector}\" ({} -> {}).",
        from.label(),
        to.label()
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use nav_core::cli::{AuthMode, SandboxMode, Transport};
    use nav_core::{AskForApproval, EventStream, ResponsesTransport};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::Pin;
    use tokio::sync::mpsc;

    struct NoopTransport;

    impl ResponsesTransport for NoopTransport {
        fn create<'a>(
            &'a self,
            _body: Value,
            _events: mpsc::UnboundedSender<AgentEvent>,
        ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>> {
            Box::pin(async {
                let boxed: EventStream = Box::pin(stream::empty());
                Ok(boxed)
            })
        }
    }

    fn test_swap() -> PendingModelSwap {
        PendingModelSwap {
            selector: "ollama/llama3".into(),
            resolved: ResolvedProvider {
                base_url: "http://localhost:11434/v1".into(),
                bearer: None,
                headers: BTreeMap::new(),
                model_id: "llama3".into(),
                reasoning_effort: None,
                max_output_tokens: None,
                display_name: "Ollama (local)/llama3".into(),
            },
        }
    }

    fn test_args() -> Args {
        Args {
            model: "gpt-5.5".into(),
            auth: AuthMode::Chatgpt,
            transport: Transport::Websocket,
            codex_home: None,
            max_turns: 4,
            tool_call_soft_budget: 0,
            bash_timeout_secs: 10,
            idle_timeout_secs: 30,
            resume: None,
            list_sessions: false,
            pick_session: false,
            name: None,
            cwd: None,
            db_path: None,
            json_events: false,
            json_rpc: false,
            approval_policy: AskForApproval::Never,
            sandbox: SandboxMode::DangerFullAccess,
            dangerously_bypass_approvals_and_sandbox: false,
            auto_compact_token_limit: 0,
            auto_compact_fraction: 1.0,
            ambient_context_token_budget: 0,
            git_checkpoints: false,
            no_git_checkpoints: false,
            reasoning_effort: None,
            command: None,
            prompt: vec![],
        }
    }

    #[test]
    fn pending_model_swap_applies_to_transport_args_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let db_path = dir.path().join("nav.sqlite");
        let store = SessionStore::open(Some(db_path)).unwrap();
        let session_id = store
            .create_session(&cwd, PROVIDER_OPENAI_RESPONSES, "gpt-5.5", None)
            .unwrap();
        let transport = ModelTransportHandle::new(NoopTransport);
        let mut args = test_args();
        let mut chat = ChatWidget::new();
        let mut pending = Some(test_swap());

        apply_pending_model_swap(
            &mut pending,
            &transport,
            &mut args,
            &store,
            &session_id,
            &mut chat,
        );

        assert!(pending.is_none());
        assert_eq!(args.model, "ollama/llama3");
        assert_eq!(transport.wire_format(), WireFormat::ChatCompletions);
        let summary = store.session_summary(&session_id).unwrap().unwrap();
        assert_eq!(summary.model, "ollama/llama3");
    }
}
