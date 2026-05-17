//! Chat history rendering for the nav TUI.
//!
//! Defines the [`HistoryCell`] trait, concrete cell types backed by
//! [`nav_core::AgentEvent`], and the [`ChatWidget`] that stacks cells
//! top-to-bottom in a ratatui buffer.

pub mod bottom_pane;
mod cells;
mod history;
mod streaming;
mod widget;

pub use cells::{AssistantMessageCell, ErrorCell, ToolCallCell, ToolOutputCell, UserMessageCell};
pub use history::HistoryCell;
pub use streaming::StreamController;
pub use widget::ChatWidget;

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{execute, terminal};
use nav_core::{AgentEvent, SessionBinding, cli::Args, run_agent};
use std::io::{self, IsTerminal};
use std::path::Path;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};

pub async fn run(
    transport: &dyn nav_core::ResponsesTransport,
    args: &Args,
    cwd: &Path,
    prompt: &str,
    session: Option<&SessionBinding<'_>>,
    initial_input: Option<Vec<serde_json::Value>>,
) -> Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        crossterm::event::EnableBracketedPaste
    )?;
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(
            out,
            terminal::LeaveAlternateScreen,
            crossterm::event::DisableBracketedPaste
        );
        eprintln!("{info}");
    }));

    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<String>();
    let (term_tx, mut term_rx) = mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        loop {
            if let Ok(true) = crossterm::event::poll(std::time::Duration::from_millis(50))
                && let Ok(ev) = crossterm::event::read()
                && term_tx.send(ev).is_err()
            {
                break;
            }
        }
    });

    let agent_fut = run_agent(
        transport,
        args,
        cwd,
        prompt,
        agent_tx,
        session,
        initial_input,
    );
    tokio::pin!(agent_fut);

    let mut ctrl_c_count = 0;
    let mut should_quit = false;
    while !should_quit {
        tokio::select! {
            Some(ev) = term_rx.recv() => {
                if let Event::Key(key) = ev {
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        ctrl_c_count += 1;
                        if ctrl_c_count >= 2 { should_quit = true; }
                    }
                    if key.code == KeyCode::Enter {
                        let _ = app_tx.send("submit".into());
                    }
                }
            }
            Some(_event) = agent_rx.recv() => {
            }
            Some(cmd) = app_rx.recv() => {
                if cmd == "/quit" { should_quit = true; }
            }
            result = &mut agent_fut => {
                result?;
                should_quit = true;
            }
            _ = sleep(Duration::from_millis(25)) => {}
        }
    }

    disable_raw_mode()?;
    execute!(
        io::stdout(),
        terminal::LeaveAlternateScreen,
        crossterm::event::DisableBracketedPaste
    )?;
    std::panic::set_hook(prev_hook);
    if io::stdout().is_terminal() {
        eprintln!("nav-tui exited");
    }
    Ok(())
}
