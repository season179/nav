pub mod bottom_pane;
mod cells;
mod history;
mod streaming;
mod widget;

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nav_core::{
    AgentEvent, ResponsesTransport, SessionBinding, SessionStore, cli::Args, run_agent,
};
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::sync::mpsc;

pub use cells::{AssistantMessageCell, ErrorCell, ToolCallCell, ToolOutputCell, UserMessageCell};
pub use history::HistoryCell;
pub use streaming::StreamController;
pub use widget::ChatWidget;

enum AppEvent {
    Quit,
    Agent(AgentEvent),
}

pub async fn run(
    prompt: String,
    transport: impl ResponsesTransport,
    args: &Args,
    cwd: PathBuf,
    store: SessionStore,
    session_id: String,
    initial_input: Option<Vec<serde_json::Value>>,
) -> Result<()> {
    enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        event::EnableBracketedPaste
    )?;
    install_panic_hook();

    let binding = SessionBinding {
        store: &store,
        session_id,
    };
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();

    let agent = run_agent(
        &transport,
        args,
        &cwd,
        &prompt,
        agent_tx,
        Some(&binding),
        initial_input,
    );
    tokio::pin!(agent);
    let mut ctrl_c = 0u8;

    loop {
        tokio::select! {
            res = &mut agent => {
                res?;
                break;
            }
            Ok(cev) = tokio::task::spawn_blocking(event::read) => {
                if let Ok(CtEvent::Key(k)) = cev {
                    if matches!(k.code, KeyCode::Char('c')) && k.modifiers.contains(KeyModifiers::CONTROL) {
                        ctrl_c += 1;
                        if ctrl_c >= 2 { app_tx.send(AppEvent::Quit).ok(); }
                    }
                    if matches!(k.code, KeyCode::Char('q')) {
                        app_tx.send(AppEvent::Quit).ok();
                    }
                }
            }
            Some(ev) = agent_rx.recv() => {
                app_tx.send(AppEvent::Agent(ev)).ok();
            }
            Some(ev) = app_rx.recv() => {
                match ev {
                    AppEvent::Quit => break,
                    AppEvent::Agent(_ev) => {}
                }
            }
        }
    }
    restore_terminal();
    Ok(())
}

fn install_panic_hook() {
    static HOOK: OnceLock<()> = OnceLock::new();
    HOOK.get_or_init(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
    });
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        event::DisableBracketedPaste,
        LeaveAlternateScreen
    );
}
