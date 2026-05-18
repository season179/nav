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
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CtEvent, KeyCode, KeyEvent,
    KeyModifiers, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use nav_core::{
    AgentEvent, Catalog, OpenAiTransport, SessionBinding, SessionId, SessionStore, cli::Args,
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
    /// Standalone `/<skill>` — the wrapped body is held until the next
    /// non-slash prompt rather than fired as its own turn.
    QueueSkill {
        skill_name: String,
        wrapped_body: String,
    },
}

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
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

fn enter_tui(out: &mut impl io::Write) -> Result<()> {
    enable_raw_mode()?;
    if let Err(err) = crossterm::execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::event::EnableBracketedPaste
    ) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            out,
            DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
            LeaveAlternateScreen
        );
        return Err(err.into());
    }
    Ok(())
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

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = crossterm::execute!(
            out,
            DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
            LeaveAlternateScreen
        );
        prev(info);
    }));

    let slash_entries = bottom_pane::build_slash_entries(skills.as_ref());

    let mut chat = ChatWidget::new();
    if resume_events.is_empty() {
        chat.push_welcome(&args.model, cwd.display().to_string(), &session_id);
    }
    // The first prompt after `--resume` gets the rebuilt transcript. Later
    // prompts currently start from their own text; the local session log still
    // records them, but nav does not maintain server-side conversation state
    // because `response_body` sends `store: false`.
    let mut initial_input: Option<Vec<Value>> = if resume_events.is_empty() {
        None
    } else {
        Some(rebuild_responses_input(&resume_events))
    };
    for ev in resume_events {
        chat.ingest(ev);
    }
    let mut pane = bottom_pane::BottomPane::with_slash_entries(slash_entries);
    let mut ctrl_c_count = 0u8;
    // Each `run_agent` call is independent of prior turns, so a standalone
    // `/<skill>` cannot persist on its own — we hold its wrapped body here
    // and prepend it onto the next non-slash prompt.
    let mut pending_skill: Option<String> = None;
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
                        // Scrollback shows the typed text; the wrapped SKILL.md
                        // goes only to the model-facing payload.
                        let prompt = prepend_pending_skill(pending_skill.take(), &raw_prompt);
                        chat.scroll_to_bottom();
                        chat.push_user(raw_prompt);
                        turn_started_at = Some(Instant::now());
                        let transport = Arc::clone(&transport);
                        let args = args.clone();
                        let cwd = cwd.clone();
                        let store = Arc::clone(&store);
                        let session_id = session_id.clone();
                        let tx = agent_tx.clone();
                        let skills = Arc::clone(&skills);
                        // `take()` consumes the rebuilt transcript exactly once;
                        // otherwise every later prompt would resend the same
                        // pre-resume history.
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
                                skills.as_ref(),
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

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn handle_scrollback_key(
    chat: &mut ChatWidget,
    key: &KeyEvent,
    (history_w, history_h): (u16, u16),
) -> bool {
    let page_rows = history_h.saturating_sub(1).max(1);
    match key.code {
        KeyCode::PageUp => chat.scroll_up(page_rows, history_w, history_h),
        KeyCode::PageDown => chat.scroll_down(page_rows, history_w, history_h),
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
            chat.scroll_to_top(history_w, history_h)
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => chat.scroll_to_bottom(),
        _ => return false,
    }
    true
}

fn handle_mouse_scroll(
    chat: &mut ChatWidget,
    kind: MouseEventKind,
    (history_w, history_h): (u16, u16),
) {
    match kind {
        MouseEventKind::ScrollUp => chat.scroll_up(3, history_w, history_h),
        MouseEventKind::ScrollDown => chat.scroll_down(3, history_w, history_h),
        _ => {}
    }
}

fn dispatch_submit(text: String, skills: &Catalog, app_tx: &mpsc::UnboundedSender<AppEvent>) {
    if text == "/quit" || text == "/exit" {
        app_tx.send(AppEvent::Quit).ok();
    } else if text == "/clear" {
        app_tx.send(AppEvent::Clear).ok();
    } else {
        match classify_slash(&text, skills) {
            SlashAction::NotASkill => {
                app_tx.send(AppEvent::Submit(text)).ok();
            }
            SlashAction::Inline { prompt } => {
                app_tx.send(AppEvent::Submit(prompt)).ok();
            }
            SlashAction::Queue {
                skill_name,
                wrapped_body,
            } => {
                app_tx
                    .send(AppEvent::QueueSkill {
                        skill_name,
                        wrapped_body,
                    })
                    .ok();
            }
        }
    }
}

fn spinner_frame(tick: u64) -> char {
    const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[(tick as usize) % FRAMES.len()]
}

/// Classification of a submitted composer line that may be a skill activation.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashAction {
    NotASkill,
    /// Standalone `/<skill-name>`. The wrapped body should be queued and
    /// prepended to the next real prompt — sending it as its own turn would
    /// be lost, since each `run_agent` call replays no prior history.
    Queue {
        skill_name: String,
        wrapped_body: String,
    },
    /// `/<skill-name> <request>` — wrap and request travel together.
    Inline {
        prompt: String,
    },
}

/// Wraps the leading `/<skill-name>` (if any) in a `<skill name=… dir=…>`
/// block so the model can load instructions and resolve relative resources
/// against the skill's directory. Scripts/references inside the SKILL.md
/// are not read here — the model loads them on demand.
pub fn classify_slash(text: &str, skills: &Catalog) -> SlashAction {
    let trimmed = text.trim_start();
    let Some(first_token) = trimmed.split_whitespace().next() else {
        return SlashAction::NotASkill;
    };
    let Some(skill_name) = first_token.strip_prefix('/') else {
        return SlashAction::NotASkill;
    };
    let Some(skill) = skills.get(skill_name) else {
        return SlashAction::NotASkill;
    };

    let body = std::fs::read_to_string(&skill.skill_md_path).unwrap_or_else(|err| {
        format!(
            "[nav: failed to read SKILL.md for `{}` at {}: {err}]",
            skill.name,
            skill.skill_md_path.display()
        )
    });
    let wrapped_body = format!(
        "<skill name=\"{name}\" dir=\"{dir}\">\n{body}\n</skill>",
        name = skill.name,
        dir = skill.skill_dir.display(),
        body = body.trim_end()
    );

    let rest = trimmed[first_token.len()..].trim_start();
    if rest.is_empty() {
        SlashAction::Queue {
            skill_name: skill.name.clone(),
            wrapped_body,
        }
    } else {
        SlashAction::Inline {
            prompt: format!("{wrapped_body}\n\n{rest}\n"),
        }
    }
}

pub fn prepend_pending_skill(pending: Option<String>, prompt: &str) -> String {
    match pending {
        Some(body) => format!("{body}\n\n{prompt}"),
        None => prompt.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nav_core::{Catalog, Skill, SkillScope};
    use std::fs;
    use tempfile::tempdir;

    fn catalog_with_skill(dir: &std::path::Path) -> Catalog {
        let skill_dir = dir.join("foo");
        fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        fs::write(
            &skill_md,
            "---\nname: foo\ndescription: do foo\n---\nHere are instructions.\n",
        )
        .unwrap();
        Catalog::new(vec![Skill {
            name: "foo".into(),
            description: "do foo".into(),
            skill_md_path: skill_md,
            skill_dir,
            scope: SkillScope::Project,
        }])
    }

    #[test]
    fn classify_slash_queues_standalone_invocation() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        match classify_slash("/foo", &catalog) {
            SlashAction::Queue {
                skill_name,
                wrapped_body,
            } => {
                assert_eq!(skill_name, "foo");
                assert!(wrapped_body.contains("<skill name=\"foo\""));
                assert!(wrapped_body.contains("Here are instructions."));
                assert!(wrapped_body.trim_end().ends_with("</skill>"));
            }
            other => panic!("expected Queue, got {other:?}"),
        }
    }

    #[test]
    fn classify_slash_inlines_when_request_follows() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        match classify_slash("/foo please help with X", &catalog) {
            SlashAction::Inline { prompt } => {
                assert!(prompt.contains("</skill>"));
                assert!(prompt.contains("please help with X"));
            }
            other => panic!("expected Inline, got {other:?}"),
        }
    }

    #[test]
    fn classify_slash_passes_through_unknown_or_plain_text() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        assert!(matches!(
            classify_slash("/bar", &catalog),
            SlashAction::NotASkill
        ));
        assert!(matches!(
            classify_slash("plain text", &catalog),
            SlashAction::NotASkill
        ));
    }

    #[test]
    fn prepend_pending_skill_merges_body_with_prompt() {
        let merged = prepend_pending_skill(Some("<skill>body</skill>".into()), "do X");
        assert!(merged.starts_with("<skill>"));
        assert!(merged.contains("do X"));
    }

    #[test]
    fn prepend_pending_skill_returns_prompt_when_empty() {
        let merged = prepend_pending_skill(None, "do X");
        assert_eq!(merged, "do X");
    }
}
