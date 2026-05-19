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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::ChatWidget;
use crate::bottom_pane::{self, PendingApproval};
use crate::input::{AppEvent, dispatch_submit, handle_scrollback_key, is_ctrl_c};
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
    session_id: SessionId,
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
    let mut ctrl_c_count = 0u8;
    // A standalone `/<skill>` is a local TUI gesture, not a model turn. Hold
    // its wrapped body here and prepend it onto the next non-slash prompt.
    let mut pending_skill: Option<(String, String)> = None;
    // Buffered prompts submitted while a turn is in flight. Lets the user
    // keep typing during a manual compaction (or any other long turn)
    // without losing input.
    let mut queued_submissions: std::collections::VecDeque<(String, Vec<PathBuf>)> =
        std::collections::VecDeque::new();
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
    let bypass = args.dangerously_bypass_approvals_and_sandbox;
    let pending_approvals = PendingApprovals::default();
    let sandbox_policy = sandbox_policy_from_args(&args, &cwd);
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
        // `record_approval_decision` UPDATE finds no row.
        let channel = ChannelGate::new(pending_approvals.clone(), agent_tx.clone())
            .with_sink(Arc::new(store.sink_for(session_id.clone())));
        (Arc::new(channel), args.approval_policy)
    };
    let permissions = PermissionContext {
        gate,
        policy,
        sandbox: Arc::from(select_for_platform(&sandbox_policy)),
        sandbox_policy,
        // Default empty; populated when the user picks `[a]llow for session`
        // on the approval modal. Shared across spawned turns via Arc.
        session_allowlist: nav_core::permissions::SessionAllowlist::default(),
    };

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
        term.terminal.draw(|f| {
            use ratatui::layout::{Constraint, Layout};
            let area = f.area();
            let pane_h = pane
                .desired_height(area.width)
                .max(3)
                .min(area.height.saturating_sub(2));
            let chunks = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(pane_h),
                Constraint::Length(1),
            ])
            .split(area);
            history_viewport = (chunks[0].width, chunks[0].height);
            f.render_widget(&chat, chunks[0]);
            f.render_widget(&pane, chunks[1]);
            if let Some((cx, cy)) = pane.cursor_position(chunks[1]) {
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
                chunks[2],
            );
        })?;

        tokio::select! {
            Some(ev) = agent_rx.recv() => {
                if turn_is_terminal(&ev) {
                    turn_started_at = None;
                    // Drain one queued prompt now that the agent is free.
                    // We hand it back through the AppEvent loop so it goes
                    // through the same Submit path (skill-classify, spawn,
                    // display) the user would have hit if they'd typed it
                    // a moment later.
                    if let Some((text, images)) = queued_submissions.pop_front() {
                        app_tx
                            .send(AppEvent::Submit { text, images })
                            .ok();
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
                        // Drop any prompts that were buffered against the
                        // pre-clear transcript — surfacing them now would
                        // look like input the user re-typed into the fresh
                        // chat.
                        queued_submissions.clear();
                    }
                    AppEvent::QueueSkill { skill_name, wrapped_body } => {
                        // Replace any previously queued skill; selecting a new
                        // one before sending a prompt should override.
                        pending_skill = Some((skill_name.clone(), wrapped_body));
                        chat.scroll_to_bottom();
                        chat.push_user(format!("/{skill_name}"));
                        chat.push_skill(skill_name, "queued for the next prompt");
                    }
                    AppEvent::Submit {
                        text: raw_prompt,
                        images,
                    } => {
                        // Turn already in flight: queue the new submission
                        // and surface it as a pending line so the user can
                        // see it landed.
                        if turn_started_at.is_some() {
                            chat.scroll_to_bottom();
                            chat.push_user(format!("(queued) {raw_prompt}"));
                            queued_submissions.push_back((raw_prompt, images));
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
                            permissions: permissions.clone(),
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
                    if let Some((approval_id, decision)) =
                        pane.take_approval_decision()
                    {
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
}
