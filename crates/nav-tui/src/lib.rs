//! Chat history rendering for the nav TUI.

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
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event as CtEvent, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nav_core::{
    AgentEvent, PROVIDER_OPENAI_RESPONSES, SessionBinding, SessionStore, TurnUsage, cli::Args,
    rebuild_responses_input, run_agent,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::Path;
use tokio::sync::mpsc;

pub enum AppEvent {
    Submit(String),
    Clear,
    Quit,
}

struct TerminalGuard;
impl TerminalGuard {
    fn install() -> Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        out.execute(EnterAlternateScreen)?;
        out.execute(crossterm::event::EnableBracketedPaste)?;
        Ok(Self)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = io::stdout().execute(crossterm::event::DisableBracketedPaste);
        let _ = io::stdout().execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

pub async fn run(
    transport: &dyn nav_core::ResponsesTransport,
    args: &Args,
    cwd: &Path,
    store: &SessionStore,
) -> Result<()> {
    let _guard = TerminalGuard::install()?;
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        let _ = io::stdout().execute(crossterm::event::DisableBracketedPaste);
        let _ = io::stdout().execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
        prev(i);
    }));

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let session_id = match args.resume.as_deref() {
        Some(id) => id.to_string(),
        None => {
            store.create_session(cwd, PROVIDER_OPENAI_RESPONSES, &args.model, Some("default"))?
        }
    };
    let history = if args.resume.is_some() {
        store.load_session(&session_id)?
    } else {
        Vec::new()
    };
    let initial_input = if args.resume.is_some() {
        Some(rebuild_responses_input(&history))
    } else {
        None
    };

    let mut widget = ChatWidget::new();
    for e in history {
        widget.ingest(e);
    }
    let mut pane = bottom_pane::BottomPane::new();
    let mut ctrl_c = 0;
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    loop {
        terminal.draw(|f| {
            let area = f.area();
            f.render_widget(&widget, area);
        })?;
        tokio::select! {
            Some(ev)=agent_rx.recv()=>{ widget.ingest(ev); }
            Some(aev)=app_rx.recv()=>{ match aev { AppEvent::Quit=> break, AppEvent::Clear=>{ store.complete_turn(&session_id,&args.model,&TurnUsage::default(),None)?; widget=ChatWidget::new(); }, AppEvent::Submit(prompt)=>{
                widget.push_user(prompt.clone()); let b=SessionBinding{store,session_id:session_id.clone()}; run_agent(transport,args,cwd,&prompt,agent_tx.clone(),Some(&b),initial_input.clone()).await?;
            }} }
            Ok(ct)=tokio::task::spawn_blocking(event::read)=>{ let ct=ct?; if let CtEvent::Key(key)=ct { if key.code==KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL){ ctrl_c+=1; if ctrl_c>=2 { app_tx.send(AppEvent::Quit).ok(); } } else { ctrl_c=0; match pane.handle_key(key){ bottom_pane::ComposerEvent::Submit(s)=>{ if s=="/quit" {app_tx.send(AppEvent::Quit).ok();} else if s=="/clear" {app_tx.send(AppEvent::Clear).ok();} else {app_tx.send(AppEvent::Submit(s)).ok();}}, bottom_pane::ComposerEvent::Nothing=>{}, bottom_pane::ComposerEvent::Cancelled=>{} }}} }
        }
    }
    store.complete_turn(&session_id, &args.model, &TurnUsage::default(), None)?;
    Ok(())
}
