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

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nav_core::{AgentEvent, OpenAiTransport, SessionBinding, cli::Args, run_agent};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

pub use cells::{AssistantMessageCell, ErrorCell, ToolCallCell, ToolOutputCell, UserMessageCell};
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
    transport: &OpenAiTransport,
    args: &Args,
    cwd: &Path,
    binding: SessionBinding<'_>,
    resume_events: Vec<AgentEvent>,
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
    for ev in resume_events {
        chat.ingest(ev);
    }
    let mut pane = bottom_pane::BottomPane::new();
    let mut ctrl_c_count = 0u8;
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    loop {
        term.terminal.draw(|f| {
            use ratatui::layout::{Constraint, Layout};
            let area = f.area();
            let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(4)]).split(area);
            f.render_widget(&chat, chunks[0]);
            f.render_widget(&pane, chunks[1]);
        })?;

        tokio::select! {
            Some(ev) = agent_rx.recv() => { chat.ingest(ev); }
            Some(app) = app_rx.recv() => {
                match app {
                    AppEvent::Quit => break,
                    AppEvent::Clear => { chat = ChatWidget::new(); }
                    AppEvent::Submit(prompt) => {
                        chat.push_user(prompt.clone());
                        run_agent(transport, args, cwd, &prompt, agent_tx.clone(), Some(&binding), None).await?;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if event::poll(Duration::from_millis(0))? && let CtEvent::Key(key) = event::read()? {
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
