//! Chat history rendering for the nav TUI.
//!
//! Defines the [`HistoryCell`] trait, concrete cell types backed by
//! [`nav_core::AgentEvent`], and the [`ChatWidget`] that stacks cells
//! top-to-bottom in a ratatui buffer.

pub mod bottom_pane;
mod cells;
mod history;
mod status_bar;
mod streaming;
mod theme;
mod widget;

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nav_core::{
    AgentEvent, OpenAiTransport, SessionBinding, SessionId, SessionStore, cli::Args,
    rebuild_responses_input, run_agent,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use serde_json::Value;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use status_bar::{AgentState, StatusBar};

pub use cells::{
    AssistantMessageCell, ErrorCell, ToolCallCell, ToolOutputCell, UserMessageCell, WelcomeCell,
};
pub use history::HistoryCell;
pub use streaming::StreamController;
pub use widget::ChatWidget;

#[derive(Debug)]
enum AppEvent {
    Submit(String),
    Quit,
    Clear,
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            crossterm::event::DisableBracketedPaste
        );
        let _ = self.terminal.show_cursor();
    }
}

pub async fn run(
    transport: Arc<OpenAiTransport>,
    args: Args,
    cwd: PathBuf,
    store: Arc<SessionStore>,
    session_id: SessionId,
    resume_events: Vec<AgentEvent>,
    initial_prompt: Option<String>,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    let mut term = TerminalGuard { terminal };

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = crossterm::execute!(
            out,
            LeaveAlternateScreen,
            crossterm::event::DisableBracketedPaste
        );
        prev(info);
    }));

    let mut chat = ChatWidget::new();
    if resume_events.is_empty() {
        chat.push_welcome(&args.model, cwd.display().to_string(), &session_id);
    }
    // First turn after `--resume` must rehydrate the Responses transcript so
    // the model sees prior context; subsequent turns are appended server-side.
    let mut initial_input: Option<Vec<Value>> = if resume_events.is_empty() {
        None
    } else {
        Some(rebuild_responses_input(&resume_events))
    };
    for ev in resume_events {
        chat.ingest(ev);
    }
    let mut pane = bottom_pane::BottomPane::new();
    let mut ctrl_c_count = 0u8;
    let mut turn_started_at: Option<Instant> = None;
    let mut spinner_tick: u64 = 0;
    let cwd_short = status_bar::shorten_home(&cwd);
    let branch = status_bar::git_branch(&cwd);
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
            f.render_widget(&chat, chunks[0]);
            f.render_widget(&pane, chunks[1]);
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
                chat.ingest(ev);
            }
            Some(app) = app_rx.recv() => {
                match app {
                    AppEvent::Quit => break,
                    AppEvent::Clear => {
                        chat = ChatWidget::new();
                        chat.push_welcome(&args.model, cwd.display().to_string(), &session_id);
                    }
                    AppEvent::Submit(prompt) => {
                        if turn_started_at.is_some() {
                            chat.ingest(AgentEvent::Error {
                                message: "agent is busy; wait for the current turn to finish".into(),
                            });
                            continue;
                        }
                        chat.push_user(prompt.clone());
                        turn_started_at = Some(Instant::now());
                        let transport = Arc::clone(&transport);
                        let args = args.clone();
                        let cwd = cwd.clone();
                        let store = Arc::clone(&store);
                        let session_id = session_id.clone();
                        let tx = agent_tx.clone();
                        let first_input = initial_input.take();
                        tokio::spawn(async move {
                            let binding = SessionBinding {
                                store: store.as_ref(),
                                session_id,
                            };
                            if let Err(err) = run_agent(
                                transport.as_ref(),
                                &args,
                                &cwd,
                                &prompt,
                                tx.clone(),
                                Some(&binding),
                                first_input,
                            )
                            .await
                            {
                                let _ = tx.send(AgentEvent::Error {
                                    message: format!("{err:#}"),
                                });
                            }
                        });
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(80)) => {
                spinner_tick = spinner_tick.wrapping_add(1);
                while event::poll(Duration::from_millis(0))? {
                    let CtEvent::Key(key) = event::read()? else { continue };
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        ctrl_c_count += 1;
                        if ctrl_c_count >= 2 { app_tx.send(AppEvent::Quit).ok(); }
                        continue;
                    }
                    ctrl_c_count = 0;
                    match pane.handle_key(key) {
                        bottom_pane::ComposerEvent::Submit(text) => {
                            if text == "/quit" { app_tx.send(AppEvent::Quit).ok(); }
                            else if text == "/clear" { app_tx.send(AppEvent::Clear).ok(); }
                            else { app_tx.send(AppEvent::Submit(text)).ok(); }
                        }
                        bottom_pane::ComposerEvent::Nothing | bottom_pane::ComposerEvent::Cancelled => {}
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
