use anyhow::Result;
use crossterm::event::{self, Event as CtEvent};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nav_core::permissions::approval::{ApprovalGate, ChannelGate, PendingApprovals};
use nav_core::sandbox::select_for_platform;
use nav_core::tools::PermissionContext;
use nav_core::{
    AgentEvent, Catalog, OpenAiTransport, ProjectContext, SessionId, SessionStore,
    cli::{Args, sandbox_policy_from_args},
    shorten_home,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::ChatWidget;
use crate::bottom_pane::{self, PendingApproval};
use crate::input::{AppEvent, dispatch_submit, handle_scrollback_key, is_ctrl_c};
use crate::pending_input::{PendingFollowUp, PendingQueue, QueuedSkill};
use crate::pending_queue_widget::PendingQueueView;
use crate::status_bar::{AgentState, StatusBar};
use crate::turn::{TurnSpawn, spawn_turn};

/// Restores the terminal to a sane state when `run` returns.
///
/// Raw mode, the alt screen, bracketed paste, and a hidden cursor would
/// otherwise persist after the process exits. `Drop` runs on normal `Ok`/`Err`
/// returns and on unwinding panics. The companion panic hook below repeats the
/// same teardown before the panic message is printed.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        leave_tui(self.terminal.backend_mut());
        let _ = self.terminal.show_cursor();
    }
}

fn leave_tui(out: &mut impl io::Write) {
    let _ = disable_raw_mode();
    let _ = write_tui_leave_sequences(out);
}

fn enter_tui(out: &mut impl io::Write) -> Result<()> {
    enable_raw_mode()?;
    if let Err(err) = write_tui_enter_sequences(out) {
        leave_tui(out);
        return Err(err.into());
    }
    Ok(())
}

fn write_tui_enter_sequences(out: &mut impl io::Write) -> io::Result<()> {
    crossterm::execute!(
        out,
        EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste
    )
}

fn write_tui_leave_sequences(out: &mut impl io::Write) -> io::Result<()> {
    crossterm::execute!(
        out,
        crossterm::event::DisableBracketedPaste,
        LeaveAlternateScreen
    )
}

fn install_panic_teardown_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        leave_tui(&mut out);
        prev(info);
    }));
}

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
    project: Arc<ProjectContext>,
) -> Result<()> {
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    let mut term = TerminalGuard { terminal };
    enter_tui(term.terminal.backend_mut())?;
    install_panic_teardown_hook();

    let slash_entries = bottom_pane::build_slash_entries(skills.as_ref());
    // Walk the workspace once at startup so the `@file` popup has something to
    // fuzzy-match against. A re-scan affordance can come later; an idle TUI
    // doesn't need a filesystem watcher to earn its keep.
    let mention_entries = bottom_pane::build_mention_entries(&cwd);

    let branch_summary = project.branch_summary();
    let context_summary = project.context_summary();
    let settings_summary = project.settings_summary(&cwd);

    let mut chat = ChatWidget::new();
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
    let mut pane =
        bottom_pane::BottomPane::with_entries(slash_entries, mention_entries, cwd.clone());
    if args.pick_session {
        open_session_picker(&store, &mut pane, Some(&session_id), &mut chat);
    }
    let mut ctrl_c_count = 0u8;
    // A standalone `/<skill>` is a local TUI gesture, not a model turn. Hold
    // its wrapped body here and prepend it onto the next non-slash prompt.
    let mut pending_skill: Option<(String, String)> = None;
    let mut turn_started_at: Option<Instant> = None;
    // Follow-ups submitted while a turn is already in flight. Drained FIFO
    // after the active turn settles so the session never runs two concurrent
    // turns against the same store.
    let mut pending_queue = PendingQueue::new();
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
    // The per-turn handles currently in flight. Held outside the spawn so
    // the input path can interact with them (trip abort, submit steering)
    // without needing access to the spawned task.
    let mut active_abort: Option<nav_core::AbortSignal> = None;
    let mut active_steering: Option<nav_core::SteeringQueue> = None;

    if let Some(prompt) = initial_prompt {
        app_tx
            .send(AppEvent::Submit {
                text: prompt,
                images: Vec::new(),
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
        let mut history_viewport = (1, 1);
        let queue_previews = pending_queue.previews();
        let steering_pending = active_steering.as_ref().map(|q| q.len()).unwrap_or(0);
        term.terminal.draw(|f| {
            use ratatui::layout::{Constraint, Layout};
            let area = f.area();
            let pane_h = pane
                .desired_height(area.width)
                .max(3)
                .min(area.height.saturating_sub(2));
            let queue_view = PendingQueueView::new(&queue_previews).with_steering(steering_pending);
            // Cap queue height so the composer always keeps room. The min(…)
            // call guards against a tiny terminal where the queue alone would
            // squeeze the history viewport to zero rows.
            let queue_h_raw = queue_view.desired_height();
            let queue_h = queue_h_raw.min(area.height.saturating_sub(pane_h + 2));
            let chunks = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(queue_h),
                Constraint::Length(pane_h),
                Constraint::Length(1),
            ])
            .split(area);
            history_viewport = (chunks[0].width, chunks[0].height);
            f.render_widget(&chat, chunks[0]);
            if queue_h > 0 {
                f.render_widget(queue_view, chunks[1]);
            }
            f.render_widget(&pane, chunks[2]);
            if let Some((cx, cy)) = pane.cursor_position(chunks[2]) {
                f.set_cursor_position((cx, cy));
            }
            f.render_widget(
                StatusBar {
                    model: &args.model,
                    cwd_short: &cwd_short,
                    branch: branch.as_deref(),
                    dirty,
                    state,
                },
                chunks[3],
            );
        })?;

        tokio::select! {
            Some(ev) = agent_rx.recv() => {
                let turn_settled = turn_is_terminal(&ev);
                if turn_settled {
                    turn_started_at = None;
                    // Drop the per-turn abort + steering handles. The
                    // next spawn will create fresh ones so an Esc /
                    // `/steer` arriving after the model has already
                    // settled is a no-op, not an accidental abort or
                    // injection into the next turn before it starts.
                    active_abort = None;
                    // Drain any steering message that landed between the
                    // runner's final drain and the TUI receiving this
                    // settle event. Without this rescue, `/steer` typed
                    // in that tight window would sit in the queue until
                    // we drop it below, vanishing without ever reaching
                    // the model. Convert leftovers into follow-ups
                    // *without* consuming `pending_skill` — a slash
                    // skill the operator selected separately is bound
                    // to their next typed prompt, not to a rescued
                    // steering message.
                    if let Some(queue) = active_steering.take() {
                        for msg in queue.drain() {
                            let images: Vec<PathBuf> = msg
                                .attachments
                                .into_iter()
                                .map(|nav_core::UserAttachment::Image { path }| path)
                                .collect();
                            pending_queue.enqueue(msg.text.clone(), images, None);
                            chat.scroll_to_bottom();
                            chat.push_user(format!("[steering→queued] {}", msg.text));
                        }
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

                if turn_settled {
                    // Drain the next queued follow-up, if any, now that the
                    // session is free again. Serializing here is load-bearing:
                    // running two `run_agent` calls against the same session
                    // store would race the transcript and double-charge the
                    // model.
                    drain_next_queued(
                        &mut pending_queue,
                        &mut chat,
                        &mut turn_started_at,
                        &mut active_abort,
                        &mut active_steering,
                        &transport,
                        &args,
                        &cwd,
                        &store,
                        &session_id,
                        &agent_tx,
                        &skills,
                        &project,
                        &permissions,
                    );
                }
            }
            Some(app) = app_rx.recv() => {
                match app {
                    AppEvent::Quit => break,
                    AppEvent::Clear => {
                        chat = ChatWidget::new();
                        chat.push_welcome(
                            &args.model,
                            cwd.display().to_string(),
                            &session_id,
                            branch_summary.clone(),
                            context_summary.clone(),
                            settings_summary.clone(),
                        );
                        pending_skill = None;
                        // /clear is "reset my intent". Stale queued follow-ups
                        // and pending steering would both surprise the user on
                        // the next turn — wipe both.
                        pending_queue.clear();
                        if let Some(queue) = active_steering.as_ref() {
                            queue.drain();
                        }
                    }
                    AppEvent::QueueSkill { skill_name, wrapped_body } => {
                        // Replace any previously queued skill; selecting a new
                        // one before sending a prompt should override.
                        pending_skill = Some((skill_name.clone(), wrapped_body));
                        chat.scroll_to_bottom();
                        chat.push_user(format!("/{skill_name}"));
                        chat.push_skill(skill_name, "queued for the next prompt");
                    }
                    AppEvent::Steer { text, images } => {
                        if text.trim().is_empty() {
                            chat.scroll_to_bottom();
                            chat.ingest(AgentEvent::Error {
                                message: "/steer needs a message — try `/steer <text>`".into(),
                            });
                            continue;
                        }
                        if let Some(steering) = active_steering.as_ref() {
                            // Active turn — submit into the per-turn
                            // steering queue. The runner drains and folds
                            // each message into the model's next request
                            // at a safe boundary.
                            let attachments: Vec<_> = images
                                .into_iter()
                                .map(|path| nav_core::UserAttachment::Image { path })
                                .collect();
                            steering.submit(nav_core::SteeringMessage::with_attachments(
                                text.clone(),
                                attachments,
                            ));
                            chat.scroll_to_bottom();
                            chat.push_skill("steer", "queued for next model boundary");
                            chat.push_user(format!("[steering] {text}"));
                        } else {
                            // No active turn — degrade to a normal Submit
                            // so the user's message still reaches the
                            // model rather than vanishing.
                            app_tx
                                .send(AppEvent::Submit { text, images })
                                .ok();
                        }
                    }
                    AppEvent::ListSessions => {
                        match store.list_sessions(None) {
                            Ok(summaries) => {
                                chat.scroll_to_bottom();
                                chat.push_session_list(summaries);
                            }
                            Err(err) => chat.ingest(AgentEvent::Error {
                                message: format!("{err:#}"),
                            }),
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
                                chat = ChatWidget::new();
                                for event in events {
                                    chat.ingest(event);
                                }
                                chat.push_session_notice(
                                    "resume",
                                    format!("Resumed session {session_id}"),
                                );
                            }
                            Err(err) => chat.ingest(AgentEvent::Error {
                                message: format!("{err:#}"),
                            }),
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
                            Err(err) => chat.ingest(AgentEvent::Error {
                                message: format!("{err:#}"),
                            }),
                        }
                    }
                    AppEvent::Export { path } => {
                        match export_current_session(&store, &session_id, &cwd, path) {
                            Ok(path) => chat.push_session_notice(
                                "export",
                                format!("Wrote transcript to {}", path.display()),
                            ),
                            Err(err) => chat.ingest(AgentEvent::Error {
                                message: format!("{err:#}"),
                            }),
                        }
                    }
                    AppEvent::SlashError { message } => {
                        chat.ingest(AgentEvent::Error { message });
                    }
                    AppEvent::Submit {
                        text: raw_prompt,
                        images,
                    } => {
                        // Active turn → queue the follow-up instead of racing
                        // a second `run_agent`. Drain happens when the active
                        // turn settles.
                        if turn_started_at.is_some() {
                            enqueue_busy_submit(
                                &mut pending_queue,
                                &mut pending_skill,
                                &mut chat,
                                raw_prompt,
                                images,
                            );
                            continue;
                        }

                        let pending_skill_name =
                            pending_skill.as_ref().map(|(name, _)| name.clone());
                        let pending_skill_body =
                            pending_skill.as_ref().map(|(_, body)| body.as_str());
                        let attachments = images
                            .into_iter()
                            .map(|path| nav_core::UserAttachment::Image { path })
                            .collect();
                        // Fresh per-turn abort signal + steering queue.
                        // Holding the clones here lets the key handler
                        // trip the abort and the `/steer` command push
                        // into the queue; the spawned `run_agent` checks
                        // both at safe boundaries.
                        let turn_abort = nav_core::AbortSignal::new();
                        let turn_steering = nav_core::SteeringQueue::new();
                        let mut turn_permissions = permissions.clone();
                        turn_permissions.abort = turn_abort.clone();
                        turn_permissions.steering = turn_steering.clone();
                        let spawned = spawn_turn(TurnSpawn {
                            transport: Arc::clone(&transport),
                            args: args.clone(),
                            cwd: cwd.clone(),
                            store: Arc::clone(&store),
                            session_id: session_id.clone(),
                            raw_prompt: raw_prompt.clone(),
                            pending_skill: pending_skill_body,
                            attachments,
                            agent_tx: agent_tx.clone(),
                            skills: Arc::clone(&skills),
                            project: Arc::clone(&project),
                            permissions: turn_permissions,
                        });
                        if let Err(err) = spawned {
                            chat.scroll_to_bottom();
                            chat.ingest(AgentEvent::Error {
                                message: format!("{err:#}"),
                            });
                            continue;
                        }

                        pending_skill = None;
                        chat.scroll_to_bottom();
                        if let Some(skill_name) = pending_skill_name {
                            chat.push_skill(skill_name, "applied to this turn");
                        }
                        chat.push_user(raw_prompt);
                        turn_started_at = Some(Instant::now());
                        active_abort = Some(turn_abort);
                        active_steering = Some(turn_steering);
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
                                ctrl_c_count += 1;
                                if ctrl_c_count >= 2 {
                                    app_tx.send(AppEvent::Quit).ok();
                                }
                                continue;
                            }
                            ctrl_c_count = 0;
                            // Esc while a turn is active and no overlay is up
                            // is the abort gesture. Overlays (slash popup,
                            // mention popup, approval modal) own Esc for
                            // their own cancel semantics. With no active
                            // turn, Esc falls through to the composer
                            // (returns `Cancelled`, which the loop ignores).
                            if is_abort_key(&key)
                                && !pane.has_overlay()
                                && let Some(abort) = active_abort.as_ref()
                            {
                                abort.trip("user pressed esc");
                                continue;
                            }
                            // Queue controls. These intercept above the
                            // composer because `Composer::handle_key` already
                            // no-ops on Ctrl-modified character keys, so the
                            // bindings don't clash with bash-style editing.
                            if is_queue_edit_last(&key)
                                && !pane.has_overlay()
                                && let Some(last) = pending_queue.pop_last()
                            {
                                restore_for_edit(
                                    &mut pane,
                                    &mut pending_skill,
                                    &mut chat,
                                    last,
                                );
                                continue;
                            }
                            if is_queue_clear(&key) && !pane.has_overlay() {
                                let had_follow_ups = !pending_queue.is_empty();
                                let steering_count = active_steering
                                    .as_ref()
                                    .map(|q| q.len())
                                    .unwrap_or(0);
                                if had_follow_ups || steering_count > 0 {
                                    pending_queue.clear();
                                    if let Some(queue) = active_steering.as_ref() {
                                        queue.drain();
                                    }
                                    chat.scroll_to_bottom();
                                    chat.ingest(AgentEvent::Error {
                                        message: "pending queue cleared".into(),
                                    });
                                    continue;
                                }
                            }
                            if handle_scrollback_key(&mut chat, &key, history_viewport) {
                                continue;
                            }
                            match pane.handle_key(key) {
                                bottom_pane::ComposerEvent::Submit { text, images } => {
                                    dispatch_submit(text, images, skills.as_ref(), &app_tx);
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
                        // Persist before signalling the agent so the row
                        // is there if the user inspects --list-sessions
                        // mid-turn.
                        if let Err(err) = store.record_approval_decision(
                            &session_id,
                            &approval_id,
                            decision.as_str(),
                        ) {
                            eprintln!("nav-tui: failed to record approval: {err:#}");
                        }
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

fn spinner_frame(tick: u64) -> char {
    const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[(tick as usize) % FRAMES.len()]
}

fn is_queue_edit_last(key: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_queue_clear(key: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    key.code == KeyCode::Char('x') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_abort_key(key: &crossterm::event::KeyEvent) -> bool {
    // Single Esc with no modifiers. Two-tap Ctrl+C remains the quit
    // gesture, and a typical Esc inside a popup still routes through the
    // overlay's own cancel handler at the call site.
    use crossterm::event::KeyCode;
    key.code == KeyCode::Esc && key.modifiers.is_empty()
}

/// Capture a submit issued during an active turn into the follow-up queue.
/// Snapshots the currently selected slash-skill so a later `/skill2` doesn't
/// retroactively rewrite the activation of an earlier queued item.
fn enqueue_busy_submit(
    queue: &mut PendingQueue,
    pending_skill: &mut Option<(String, String)>,
    chat: &mut ChatWidget,
    raw_prompt: String,
    images: Vec<PathBuf>,
) -> u64 {
    let skill_snapshot = pending_skill
        .take()
        .map(|(name, wrapped_body)| QueuedSkill { name, wrapped_body });
    let skill_name_for_log = skill_snapshot.as_ref().map(|s| s.name.clone());
    let id = queue.enqueue(raw_prompt.clone(), images, skill_snapshot);
    chat.scroll_to_bottom();
    if let Some(skill_name) = skill_name_for_log {
        chat.push_skill(skill_name, "queued as a follow-up");
    }
    chat.push_user(format!("[queued] {raw_prompt}"));
    id
}

fn restore_for_edit(
    pane: &mut bottom_pane::BottomPane,
    pending_skill: &mut Option<(String, String)>,
    chat: &mut ChatWidget,
    item: PendingFollowUp,
) {
    pane.set_composer_text(&item.text);
    *pending_skill = item.skill.map(|s| (s.name, s.wrapped_body));
    chat.scroll_to_bottom();
    chat.ingest(AgentEvent::Error {
        message: "edit queued follow-up — press Enter to resubmit".into(),
    });
}

/// Spawn the next queued follow-up after a turn settles. Mirrors the inline
/// submit path so attachments, skill activation, and the visible transcript
/// stay in lockstep with the synchronous case.
#[allow(clippy::too_many_arguments)]
fn drain_next_queued(
    queue: &mut PendingQueue,
    chat: &mut ChatWidget,
    turn_started_at: &mut Option<Instant>,
    active_abort: &mut Option<nav_core::AbortSignal>,
    active_steering: &mut Option<nav_core::SteeringQueue>,
    transport: &Arc<OpenAiTransport>,
    args: &Args,
    cwd: &std::path::Path,
    store: &Arc<SessionStore>,
    session_id: &SessionId,
    agent_tx: &mpsc::UnboundedSender<AgentEvent>,
    skills: &Arc<Catalog>,
    project: &Arc<ProjectContext>,
    permissions: &PermissionContext,
) {
    let Some(item) = queue.drain_next() else {
        return;
    };
    let PendingFollowUp {
        text,
        attachments,
        skill,
        ..
    } = item;
    let (skill_name, skill_body) = match skill {
        Some(s) => (Some(s.name), Some(s.wrapped_body)),
        None => (None, None),
    };
    let turn_abort = nav_core::AbortSignal::new();
    let turn_steering = nav_core::SteeringQueue::new();
    let mut turn_permissions = permissions.clone();
    turn_permissions.abort = turn_abort.clone();
    turn_permissions.steering = turn_steering.clone();
    let spawned = spawn_turn(TurnSpawn {
        transport: Arc::clone(transport),
        args: args.clone(),
        cwd: cwd.to_path_buf(),
        store: Arc::clone(store),
        session_id: session_id.clone(),
        raw_prompt: text.clone(),
        pending_skill: skill_body.as_deref(),
        attachments,
        agent_tx: agent_tx.clone(),
        skills: Arc::clone(skills),
        project: Arc::clone(project),
        permissions: turn_permissions,
    });
    if let Err(err) = spawned {
        chat.scroll_to_bottom();
        chat.ingest(AgentEvent::Error {
            message: format!("{err:#}"),
        });
        return;
    }
    chat.scroll_to_bottom();
    if let Some(name) = skill_name {
        chat.push_skill(name, "applied to this turn");
    }
    chat.push_user(text);
    *turn_started_at = Some(Instant::now());
    *active_abort = Some(turn_abort);
    *active_steering = Some(turn_steering);
}

fn build_tui_permissions(
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
            Arc::new(nav_core::permissions::approval::AutoGate::approving()),
            // Force off `Never` so the gate is consulted instead of being
            // short-circuited to a refusal by `auto_denies_approvals`.
            nav_core::permissions::AskForApproval::OnRequest,
        )
    } else {
        // Attach the session store as a durable sink so the approval
        // request hits the SQLite audit table — without it, the later
        // `record_approval_decision` UPDATE finds no row. Rebuilt on TUI
        // resume so approvals are recorded against the active session.
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
        session_allowlist: nav_core::permissions::SessionAllowlist::default(),
        abort: nav_core::AbortSignal::default(),
        steering: nav_core::SteeringQueue::default(),
    }
}

fn resume_session(store: &SessionStore, query: &str) -> Result<(SessionId, Vec<AgentEvent>)> {
    let session_id = store.resolve_session_id(query)?;
    let events = store.load_session(&session_id)?;
    Ok((session_id, events))
}

fn open_session_picker(
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
        Err(err) => chat.ingest(AgentEvent::Error {
            message: format!("{err:#}"),
        }),
    }
}

fn export_current_session(
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

/// Returns true when `ev` marks the end of an in-flight TUI turn so the
/// composer can be re-enabled and one queued prompt drained.
///
/// Manual `/compact` exits `run_agent` after `CompactionCompleted` /
/// `CompactionFailed` without an accompanying `TurnComplete`, so without
/// these arms the TUI would never clear `turn_started_at` and queued
/// prompts would pile up indefinitely. Auto-compaction is deliberately
/// excluded — it is followed by the real user turn inside the same
/// `run_agent` call, and that turn emits its own `TurnComplete`.
fn turn_is_terminal(ev: &AgentEvent) -> bool {
    matches!(
        ev,
        AgentEvent::TurnComplete { .. }
            | AgentEvent::Error { .. }
            | AgentEvent::TurnAborted { .. }
            | AgentEvent::CompactionCompleted {
                trigger: nav_core::CompactionTrigger::Manual,
                ..
            }
            | AgentEvent::CompactionFailed {
                trigger: nav_core::CompactionTrigger::Manual,
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    use nav_core::CompactionTrigger;

    #[test]
    fn turn_is_terminal_for_turn_complete_and_error() {
        assert!(turn_is_terminal(&AgentEvent::TurnComplete {
            usage: nav_core::TurnUsage::default()
        }));
        assert!(turn_is_terminal(&AgentEvent::Error {
            message: "x".into()
        }));
    }

    #[test]
    fn turn_is_terminal_for_manual_compaction_lifecycle() {
        // Manual /compact exits run_agent without a TurnComplete, so the
        // TUI must also accept its lifecycle events as turn-terminal —
        // otherwise turn_started_at never clears and queued prompts
        // pile up forever.
        assert!(turn_is_terminal(&AgentEvent::CompactionCompleted {
            trigger: CompactionTrigger::Manual,
            summary: "s".into(),
            replaced_events: 0,
            tokens_before: 0,
        }));
        assert!(turn_is_terminal(&AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            message: "x".into(),
        }));
    }

    #[test]
    fn turn_is_terminal_excludes_auto_compaction_lifecycle() {
        // Auto compaction is followed by the real user turn inside the
        // same run_agent call; that turn emits TurnComplete and drains
        // the queue. If auto-compaction events were terminal we would
        // double-drain.
        assert!(!turn_is_terminal(&AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            tokens_before: 0,
        }));
        assert!(!turn_is_terminal(&AgentEvent::CompactionCompleted {
            trigger: CompactionTrigger::Auto,
            summary: "s".into(),
            replaced_events: 0,
            tokens_before: 0,
        }));
        assert!(!turn_is_terminal(&AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Auto,
            message: "x".into(),
        }));
    }

    #[test]
    fn tui_enter_sequences_do_not_enable_mouse_capture() {
        let mut out = Vec::new();
        write_tui_enter_sequences(&mut out).unwrap();
        let bytes = String::from_utf8_lossy(&out);

        assert!(bytes.contains("\u{1b}[?1049h"));
        assert!(bytes.contains("\u{1b}[?2004h"));
        for seq in [
            "\u{1b}[?1000h",
            "\u{1b}[?1002h",
            "\u{1b}[?1003h",
            "\u{1b}[?1015h",
            "\u{1b}[?1006h",
        ] {
            assert!(
                !bytes.contains(seq),
                "mouse capture prevents native terminal text selection: {seq:?}"
            );
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Press)
    }

    #[test]
    fn enqueue_busy_submit_appends_in_order_and_clears_pending_skill() {
        let mut queue = PendingQueue::new();
        let mut chat = ChatWidget::new();
        let mut pending_skill: Option<(String, String)> = None;

        enqueue_busy_submit(
            &mut queue,
            &mut pending_skill,
            &mut chat,
            "first".into(),
            vec![],
        );
        // Selecting a skill before queueing the second prompt — the snapshot
        // must travel with this specific item and not be retroactively reset
        // when `pending_skill` changes again later.
        pending_skill = Some(("foo".into(), "<skill name=\"foo\">body</skill>".into()));
        enqueue_busy_submit(
            &mut queue,
            &mut pending_skill,
            &mut chat,
            "second".into(),
            vec![],
        );
        // `pending_skill` is now cleared so the next standalone `/skillN`
        // selection is the fresh source of truth.
        assert!(pending_skill.is_none());

        // Activating a new skill must not retroactively rewrite previously
        // queued items.
        pending_skill = Some(("bar".into(), "<skill name=\"bar\">body</skill>".into()));
        enqueue_busy_submit(
            &mut queue,
            &mut pending_skill,
            &mut chat,
            "third".into(),
            vec![],
        );

        let items: Vec<_> = queue.iter().collect();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].text, "first");
        assert!(items[0].skill.is_none());
        assert_eq!(items[1].text, "second");
        assert_eq!(items[1].skill.as_ref().unwrap().name, "foo");
        assert_eq!(items[2].text, "third");
        assert_eq!(items[2].skill.as_ref().unwrap().name, "bar");
    }

    #[test]
    fn enqueue_busy_submit_preserves_image_attachments() {
        let mut queue = PendingQueue::new();
        let mut chat = ChatWidget::new();
        let mut pending_skill: Option<(String, String)> = None;
        let images = vec![PathBuf::from(".nav/clipboard/a.png")];

        enqueue_busy_submit(
            &mut queue,
            &mut pending_skill,
            &mut chat,
            "with image".into(),
            images,
        );

        let item = queue.iter().next().unwrap();
        assert_eq!(item.images.len(), 1);
        match &item.attachments[0] {
            nav_core::UserAttachment::Image { path } => {
                assert_eq!(path, &PathBuf::from(".nav/clipboard/a.png"));
            }
        }
    }

    #[test]
    fn restore_for_edit_repopulates_composer_and_skill() {
        let mut pane = bottom_pane::BottomPane::new();
        let mut chat = ChatWidget::new();
        let mut pending_skill: Option<(String, String)> = None;
        let item = PendingFollowUp {
            id: 0,
            text: "fix wording".into(),
            images: vec![],
            attachments: vec![],
            skill: Some(QueuedSkill {
                name: "foo".into(),
                wrapped_body: "<skill name=\"foo\">body</skill>".into(),
            }),
        };

        restore_for_edit(&mut pane, &mut pending_skill, &mut chat, item);

        assert_eq!(pane.composer().text(), "fix wording");
        assert_eq!(pending_skill.as_ref().unwrap().0, "foo");
        assert!(pending_skill.as_ref().unwrap().1.contains("<skill"));
    }

    #[test]
    fn queue_edit_last_keybinding_detection() {
        assert!(is_queue_edit_last(&key(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL
        )));
        // Plain `e` is composer typing, not a queue control.
        assert!(!is_queue_edit_last(&key(
            KeyCode::Char('e'),
            KeyModifiers::NONE
        )));
        assert!(!is_queue_clear(&key(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn queue_clear_keybinding_detection() {
        assert!(is_queue_clear(&key(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_queue_clear(&key(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn abort_key_only_fires_on_bare_esc() {
        assert!(is_abort_key(&key(KeyCode::Esc, KeyModifiers::NONE)));
        // Esc with modifiers shouldn't double as abort — leave room for
        // user keybinds and let the overlay routing keep ownership.
        assert!(!is_abort_key(&key(KeyCode::Esc, KeyModifiers::CONTROL)));
        assert!(!is_abort_key(&key(KeyCode::Esc, KeyModifiers::SHIFT)));
        assert!(!is_abort_key(&key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
    }

    #[tokio::test]
    async fn pressing_abort_key_trips_the_active_signal() {
        let abort = nav_core::AbortSignal::new();
        let mut active_abort: Option<nav_core::AbortSignal> = Some(abort.clone());
        let key_event = key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(is_abort_key(&key_event));

        // Mirror the app-loop branch: when the key matches and a turn is
        // active, the handle is tripped. The signal then resolves `wait`
        // straight away so a sleeping bash future would unblock.
        if is_abort_key(&key_event)
            && let Some(handle) = active_abort.as_ref()
        {
            handle.trip("user pressed esc");
        }
        assert!(abort.is_aborted());
        assert_eq!(abort.reason().as_deref(), Some("user pressed esc"));

        // The app loop also clears `active_abort` when the turn settles,
        // so a second Esc after settle would do nothing.
        active_abort = None;
        assert!(active_abort.is_none());
    }

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
        pane.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));

        assert_eq!(pane.take_session_selection(), Some(other));
    }
}
