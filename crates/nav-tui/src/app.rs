use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event as CtEvent};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nav_core::{AgentEvent, Catalog, OpenAiTransport, SessionId, SessionStore, cli::Args};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::ChatWidget;
use crate::bottom_pane;
use crate::input::{
    AppEvent, dispatch_submit, handle_mouse_scroll, handle_scrollback_key, is_ctrl_c,
};
use crate::status_bar::{AgentState, StatusBar, git_branch, shorten_home};
use crate::turn::{TurnSpawn, spawn_turn};

/// Restores the terminal to a sane state when `run` returns.
///
/// Raw mode, the alt screen, mouse capture, bracketed paste, and a hidden
/// cursor would otherwise persist after the process exits. `Drop` runs on
/// normal `Ok`/`Err` returns and on unwinding panics. The companion panic hook
/// below repeats the same teardown before the panic message is printed.
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
    let _ = crossterm::execute!(
        out,
        DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        LeaveAlternateScreen
    );
}

fn enter_tui(out: &mut impl io::Write) -> Result<()> {
    enable_raw_mode()?;
    if let Err(err) = crossterm::execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::event::EnableBracketedPaste
    ) {
        leave_tui(out);
        return Err(err.into());
    }
    Ok(())
}

fn install_panic_teardown_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        leave_tui(&mut out);
        prev(info);
    }));
}

pub async fn run(
    transport: Arc<OpenAiTransport>,
    args: Args,
    cwd: PathBuf,
    store: Arc<SessionStore>,
    session_id: SessionId,
    resume_events: Vec<AgentEvent>,
    initial_prompt: Option<String>,
    skills: Arc<Catalog>,
) -> Result<()> {
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    let mut term = TerminalGuard { terminal };
    enter_tui(term.terminal.backend_mut())?;
    install_panic_teardown_hook();

    let slash_entries = bottom_pane::build_slash_entries(skills.as_ref());

    let mut chat = ChatWidget::new();
    if resume_events.is_empty() {
        chat.push_welcome(&args.model, cwd.display().to_string(), &session_id);
    }
    // Rehydrate the visible scrollback at startup. Each submitted turn below
    // rebuilds model-facing history fresh from the session store because
    // `response_body` sends `store: false`.
    for ev in resume_events {
        chat.ingest(ev);
    }
    let mut pane = bottom_pane::BottomPane::with_slash_entries(slash_entries);
    let mut ctrl_c_count = 0u8;
    // A standalone `/<skill>` is a local TUI gesture, not a model turn. Hold
    // its wrapped body here and prepend it onto the next non-slash prompt.
    let mut pending_skill: Option<String> = None;
    let mut turn_started_at: Option<Instant> = None;
    let mut spinner_tick: u64 = 0;
    let cwd_short = shorten_home(&cwd);
    let branch = git_branch(&cwd);
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    if let Some(prompt) = initial_prompt {
        app_tx.send(AppEvent::Submit(prompt)).ok();
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
                    state,
                },
                chunks[2],
            );
        })?;

        tokio::select! {
            Some(ev) = agent_rx.recv() => {
                if matches!(ev, AgentEvent::TurnComplete { .. } | AgentEvent::Error { .. }) {
                    turn_started_at = None;
                }
                if matches!(ev, AgentEvent::UserMessage { .. }) {
                    continue;
                }
                chat.ingest(ev);
            }
            Some(app) = app_rx.recv() => {
                match app {
                    AppEvent::Quit => break,
                    AppEvent::Clear => {
                        chat = ChatWidget::new();
                        chat.push_welcome(&args.model, cwd.display().to_string(), &session_id);
                        pending_skill = None;
                    }
                    AppEvent::QueueSkill { skill_name, wrapped_body } => {
                        // Replace any previously queued skill; selecting a new
                        // one before sending a prompt should override.
                        pending_skill = Some(wrapped_body);
                        chat.scroll_to_bottom();
                        chat.push_user(format!("/{skill_name}"));
                        chat.ingest(AgentEvent::AssistantMessageDone {
                            text: format!(
                                "Skill `{skill_name}` queued. Send your request to apply it."
                            ),
                        });
                    }
                    AppEvent::Submit(raw_prompt) => {
                        // Refuse a second prompt while a turn is still in flight.
                        // The Responses API gives no clean way to interleave two
                        // running turns on the same session, and we don't want to
                        // race the spinner / token rollups with two in-flight runs.
                        if turn_started_at.is_some() {
                            chat.scroll_to_bottom();
                            chat.ingest(AgentEvent::Error {
                                message: "agent is busy; wait for the current turn to finish".into(),
                            });
                            continue;
                        }

                        let spawned = spawn_turn(TurnSpawn {
                            transport: Arc::clone(&transport),
                            args: args.clone(),
                            cwd: cwd.clone(),
                            store: Arc::clone(&store),
                            session_id: session_id.clone(),
                            raw_prompt: raw_prompt.clone(),
                            pending_skill: pending_skill.as_deref(),
                            agent_tx: agent_tx.clone(),
                            skills: Arc::clone(&skills),
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
                                bottom_pane::ComposerEvent::Submit(text) => {
                                    dispatch_submit(text, skills.as_ref(), &app_tx);
                                }
                                bottom_pane::ComposerEvent::Nothing
                                | bottom_pane::ComposerEvent::Cancelled => {}
                            }
                        }
                        CtEvent::Mouse(mouse) => {
                            handle_mouse_scroll(&mut chat, mouse.kind, history_viewport);
                        }
                        _ => {}
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
