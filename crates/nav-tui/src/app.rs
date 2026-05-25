//! Main TUI application: event loop, agent orchestration, and rendering.
//!
//! Uses ratatui's standard Terminal with an inline viewport. History is
//! inserted into native scrollback via CSI scroll-region sequences before
//! each ratatui frame is drawn.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;
use tokio::time::Instant;

use nav_core::cli::{Args, sandbox_policy_from_args};
use nav_core::context::{Catalog, ExtensionCatalog, ProjectContext};
use nav_core::guardrails::approval::{AutoGate, ChannelGate, PendingApprovals};
use nav_core::guardrails::{
    AskForApproval, PermissionContext, ReviewDecision, SessionAllowlist, select_for_platform,
};
use nav_core::{
    AgentEvent, AgentTurnRequest, ModelTransportHandle, SessionBinding, SessionStore,
    StartupNotices, TurnUsage, run_agent,
};

use crate::insert_history;

// Spinner frames for the "Working" state.
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Bottom pane height: composer (1) + separator (1) + status (1)
const BOTTOM_PANE_HEIGHT: u16 = 3;

/// RAII guard that restores terminal state on drop.
/// Ensures raw mode and bracketed paste are cleaned up even on early returns.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste);
    }
}

// ── App state ─────────────────────────────────────────────────────

struct App {
    args: Args,
    cwd: PathBuf,
    store: Arc<SessionStore>,
    session_id: String,
    skills: Arc<Catalog>,
    extensions: Arc<ExtensionCatalog>,
    project: Arc<ProjectContext>,
    transport: ModelTransportHandle,

    // Composer state
    composer_text: String,
    composer_cursor: usize, // byte offset
    composer_placeholder: &'static str,

    // Agent turn state
    active_turn: Option<ActiveTurn>,
    pending_approvals: PendingApprovals,

    // Streaming transcript (current turn)
    streaming_text: String,
    streaming_finalized: usize,

    // Status bar
    status_state: StatusState,

    // Pending history lines to flush on next draw
    pending_history: Vec<Line<'static>>,

    // Slash popup state
    slash_popup_active: bool,
    slash_popup_filter: String,
    slash_popup_selected: usize,
    slash_suppressed: bool,

    // Transcript overlay
    transcript_active: bool,
    transcript_lines: Vec<String>,

    // Spinner frame
    spinner_idx: usize,

    // Should we exit?
    should_exit: bool,

    // Viewport tracking
    viewport_y: u16,
}

#[derive(Debug, Clone)]
enum StatusState {
    Ready,
    Working { started: Instant },
}

struct ActiveTurn {
    _handle: tokio::task::JoinHandle<()>,
}

// ── Slash commands ────────────────────────────────────────────────

struct SlashCommand {
    name: &'static str,
    description: &'static str,
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/help",
        description: "Show available commands",
    },
    SlashCommand {
        name: "/exit",
        description: "Exit nav",
    },
    SlashCommand {
        name: "/clear",
        description: "Clear scrollback",
    },
    SlashCommand {
        name: "/compact",
        description: "Compact session context",
    },
    SlashCommand {
        name: "/find",
        description: "Search skills",
    },
];

// ── Public entry point ────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run(
    transport: ModelTransportHandle,
    args: Args,
    cwd: PathBuf,
    store: Arc<SessionStore>,
    session_id: String,
    _resume_events: Vec<AgentEvent>,
    initial_prompt: Option<String>,
    skills: Arc<Catalog>,
    extensions: Arc<ExtensionCatalog>,
    project: Arc<ProjectContext>,
    startup_notices: StartupNotices,
) -> Result<()> {
    // Set up terminal with RAII guard to ensure cleanup on early returns
    crossterm::execute!(io::stdout(), EnableBracketedPaste)?;
    enable_raw_mode()?;
    let _terminal_guard = TerminalGuard;

    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;
    term.clear()?;

    // Determine initial viewport position (bottom of screen minus bottom pane)
    let size = term.size()?;
    let viewport_y = size.height.saturating_sub(BOTTOM_PANE_HEIGHT);

    let pending_approvals = PendingApprovals::default();
    let mut app = App {
        args,
        cwd,
        store,
        session_id,
        skills,
        extensions,
        project,
        transport,
        composer_text: String::new(),
        composer_cursor: 0,
        composer_placeholder: "Ask nav to do anything",
        active_turn: None,
        pending_approvals: pending_approvals.clone(),
        streaming_text: String::new(),
        streaming_finalized: 0,
        status_state: StatusState::Ready,
        pending_history: Vec::new(),
        slash_popup_active: false,
        slash_popup_filter: String::new(),
        slash_popup_selected: 0,
        slash_suppressed: false,
        transcript_active: false,
        transcript_lines: Vec::new(),
        spinner_idx: 0,
        should_exit: false,
        viewport_y,
    };

    // Render startup notices as history cells
    render_startup_notices_to_history(&mut app, &startup_notices);

    // If there's an initial prompt, submit it after first draw
    let deferred_prompt = initial_prompt;

    // Agent event channel
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // Draw initial frame
    flush_history_and_draw(&mut term, &mut app)?;

    // Submit deferred prompt if any
    if let Some(prompt) = deferred_prompt {
        app.pending_history.push(Line::from(vec![
            Span::styled(
                "> ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(prompt.clone(), Style::default().fg(Color::Green)),
        ]));
        app.pending_history.push(Line::raw(""));
        submit_prompt(&mut app, agent_tx.clone(), &prompt)?;
    }

    let mut crossterm_events = EventStream::new();
    let mut last_spinner_tick = Instant::now();

    while !app.should_exit {
        let tick_duration = Duration::from_millis(100);
        let next_tick = last_spinner_tick + tick_duration;

        tokio::select! {
            maybe_event = crossterm_events.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        handle_terminal_event(&mut app, &mut term, event, agent_tx.clone())?;
                    }
                    Some(Err(_)) | None => break,
                }
            }
            maybe_agent_event = agent_rx.recv() => {
                if let Some(event) = maybe_agent_event {
                    handle_agent_event(&mut app, event)?;
                }
            }
            _ = tokio::time::sleep_until(next_tick) => {
                if matches!(app.status_state, StatusState::Working { .. }) {
                    app.spinner_idx = (app.spinner_idx + 1) % SPINNER.len();
                }
                last_spinner_tick = Instant::now();
            }
        }

        flush_history_and_draw(&mut term, &mut app)?;
    }

    // Restore terminal (guard also handles this on early returns)
    drop(_terminal_guard);
    Ok(())
}

// ── Drawing ───────────────────────────────────────────────────────

fn flush_history_and_draw(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    // Flush pending history lines into scrollback
    if !app.pending_history.is_empty() {
        let size = term.size()?;
        let lines = std::mem::take(&mut app.pending_history);
        let viewport_bottom = app.viewport_y + BOTTOM_PANE_HEIGHT;
        insert_history::insert_history_lines(
            term.backend_mut(),
            app.viewport_y,
            viewport_bottom.min(size.height),
            size.height,
            lines,
            size.width,
        )?;
    }

    term.draw(|f| {
        let area = f.area();
        let height = area.height;

        if app.transcript_active {
            draw_transcript_overlay(f, app);
            return;
        }

        // Bottom pane takes the last BOTTOM_PANE_HEIGHT rows
        let bottom_y = height.saturating_sub(BOTTOM_PANE_HEIGHT);
        let bottom_area = Rect::new(0, bottom_y, area.width, BOTTOM_PANE_HEIGHT);
        let streaming_area = Rect::new(0, 0, area.width, bottom_y);

        // Draw streaming text in the area above the bottom pane
        if !app.streaming_text.is_empty() {
            let pending = &app.streaming_text[app.streaming_finalized..];
            let visible: Vec<Line<'_>> = pending
                .lines()
                .rev()
                .take(bottom_y as usize)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|l| {
                    Line::from(Span::styled(
                        l.to_string(),
                        Style::default().fg(Color::White),
                    ))
                })
                .collect();
            let para = Paragraph::new(visible);
            f.render_widget(para, streaming_area);
        }

        // Bottom pane: composer + separator + status
        let chunks = Layout::vertical([
            Constraint::Length(1), // composer
            Constraint::Length(1), // separator
            Constraint::Length(1), // status
        ])
        .split(bottom_area);

        draw_composer(f, app, chunks[0]);
        draw_separator(f, chunks[1]);
        draw_status_bar(f, app, chunks[2]);

        // Slash popup overlay
        if app.slash_popup_active {
            draw_slash_popup(f, app, bottom_area);
        }
    })?;

    Ok(())
}

fn draw_composer(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let display_text = if app.composer_text.is_empty() {
        app.composer_placeholder.to_string()
    } else {
        app.composer_text.clone()
    };

    let style = if app.composer_text.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };

    let line = Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(display_text, style),
    ]);

    let para = Paragraph::new(line);
    f.render_widget(para, area);

    // Position cursor inside composer
    if app.composer_text.is_empty() {
        f.set_cursor_position(Position {
            x: area.x + 2,
            y: area.y,
        });
    } else {
        let chars_before = app.composer_text[..app.composer_cursor].chars().count();
        f.set_cursor_position(Position {
            x: area.x + 2 + chars_before as u16,
            y: area.y,
        });
    }
}

fn draw_separator(f: &mut ratatui::Frame, area: Rect) {
    let separator = Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(Color::DarkGray),
    ));
    let para = Paragraph::new(separator);
    f.render_widget(para, area);
}

fn draw_status_bar(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let spinner = match &app.status_state {
        StatusState::Ready => "",
        StatusState::Working { .. } => SPINNER[app.spinner_idx],
    };

    let state_label = match &app.status_state {
        StatusState::Ready => "Ready",
        StatusState::Working { .. } => "Working",
    };

    let working_suffix = match &app.status_state {
        StatusState::Working { started } => {
            let elapsed = started.elapsed().as_secs();
            if elapsed > 0 {
                format!(" {elapsed}s")
            } else {
                String::new()
            }
        }
        StatusState::Ready => String::new(),
    };

    let spinner_prefix = if spinner.is_empty() {
        String::new()
    } else {
        format!("{} ", spinner)
    };

    let mut spans = vec![
        Span::styled("  ·  ".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}{}{}", spinner_prefix, state_label, working_suffix),
            Style::default(),
        ),
        Span::styled("  ·  ".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(&app.args.model, Style::default().fg(Color::DarkGray)),
    ];

    // Add branch info if available
    if let Some(branch) = app.project.branch_summary() {
        spans.push(Span::styled("  ·  ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(branch, Style::default().fg(Color::DarkGray)));
    }

    let line = Line::from(spans);
    let para = Paragraph::new(line).style(Style::default().bg(Color::Black));
    f.render_widget(para, area);
}

fn draw_slash_popup(f: &mut ratatui::Frame, app: &App, parent_area: Rect) {
    let filtered = filtered_slash_commands(&app.composer_text);
    let max_height = (filtered.len() as u16).min(8).min(parent_area.height / 2);
    if max_height == 0 {
        return;
    }

    let popup_area = Rect {
        x: parent_area.x,
        y: parent_area.y.saturating_sub(max_height),
        width: parent_area.width.min(40),
        height: max_height,
    };

    let items: Vec<Line<'_>> = filtered
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let style = if i == app.slash_popup_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(format!(" {:<12}", cmd.name), style),
                Span::styled(cmd.description, style),
            ])
        })
        .collect();

    let para = Paragraph::new(items).style(Style::default().bg(Color::Black));
    f.render_widget(para, popup_area);
}

fn draw_transcript_overlay(f: &mut ratatui::Frame, app: &App) {
    let area = f.area();
    let header = Line::from(vec![
        Span::styled(
            " Transcript",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default()),
        Span::styled("Esc/q close", Style::default().fg(Color::DarkGray)),
    ]);
    let para = Paragraph::new(header).style(Style::default().bg(Color::Black));
    f.render_widget(para, area);

    let content_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    let lines: Vec<Line<'_>> = if app.transcript_lines.is_empty() {
        vec![Line::from(Span::styled(
            "No transcript yet.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.transcript_lines
            .iter()
            .take(content_area.height as usize)
            .map(|l| Line::from(l.as_str()))
            .collect()
    };

    let content_para = Paragraph::new(lines).style(Style::default().bg(Color::Black));
    f.render_widget(content_para, content_area);
}

// ── Event handling ────────────────────────────────────────────────

fn handle_terminal_event(
    app: &mut App,
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    event: Event,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Result<()> {
    if app.transcript_active {
        return handle_transcript_event(app, event);
    }

    #[allow(clippy::collapsible_match)]
    match event {
        Event::Resize(_, _) => {
            let size = term.size()?;
            app.viewport_y = size.height.saturating_sub(BOTTOM_PANE_HEIGHT);
        }
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            handle_key_event(app, term, key, agent_tx)?;
        }
        Event::Paste(text) => {
            if !app.slash_popup_active {
                insert_text_at_cursor(app, &text);
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_key_event(
    app: &mut App,
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    key: KeyEvent,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Result<()> {
    if app.slash_popup_active {
        return handle_slash_popup_key(app, term, key, agent_tx);
    }

    #[allow(clippy::collapsible_match)]
    match (key.modifiers, key.code) {
        // Ctrl+C: layered behavior
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if !app.composer_text.is_empty() {
                app.composer_text.clear();
                app.composer_cursor = 0;
                return Ok(());
            }
            if app.active_turn.is_some() {
                app.pending_approvals.abort_pending();
                finalize_streaming(app);
                if let Some(active) = app.active_turn.take() {
                    active._handle.abort();
                }
                app.status_state = StatusState::Ready;
                app.pending_history.push(Line::from(Span::styled(
                    "  user interrupt - turn aborted",
                    Style::default().fg(Color::Yellow),
                )));
                return Ok(());
            }
            app.should_exit = true;
        }

        // Ctrl+L: force redraw
        (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
            // Just let the next draw happen
        }

        // Ctrl+T: transcript overlay
        (KeyModifiers::CONTROL, KeyCode::Char('t')) => {
            app.transcript_active = true;
        }

        // Enter: submit prompt
        (_, KeyCode::Enter) => {
            let text = app.composer_text.trim().to_string();
            if text.is_empty() {
                return Ok(());
            }

            // Only intercept known built-in slash commands;
            // everything else (including skill invocations like /skill-name)
            // goes to the agent as a regular prompt.
            if text.starts_with('/') {
                match text.as_str() {
                    "/help" | "/exit" | "/quit" | "/clear" | "/compact" | "/find" => {
                        return handle_slash_command(app, &text, agent_tx);
                    }
                    _ => {} // fall through to submit as prompt
                }
            }

            app.composer_text.clear();
            app.composer_cursor = 0;

            // Render user message in history with appropriate chip
            let skill_name = text.strip_prefix('/').and_then(|name| {
                if app.skills.get(name).is_some() {
                    Some(name)
                } else {
                    None
                }
            });

            if let Some(name) = skill_name {
                // Skill invocation chip: $ skill-name
                app.pending_history.push(Line::from(vec![
                    Span::styled(
                        "$ ",
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(name.to_string(), Style::default().fg(Color::Magenta)),
                ]));
            } else {
                // Regular prompt: › prompt text
                app.pending_history.push(Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(text.clone(), Style::default().fg(Color::Green)),
                ]));
            }
            app.pending_history.push(Line::raw(""));

            submit_prompt(app, agent_tx, &text)?;
        }

        // Backspace
        (_, KeyCode::Backspace) => {
            if app.composer_cursor > 0 {
                let prev = app.composer_text[..app.composer_cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                app.composer_text.drain(prev..app.composer_cursor);
                app.composer_cursor = prev;
            }
        }

        // Delete
        (_, KeyCode::Delete) => {
            if app.composer_cursor < app.composer_text.len() {
                let next = app.composer_text[app.composer_cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| app.composer_cursor + i)
                    .unwrap_or(app.composer_text.len());
                app.composer_text.drain(app.composer_cursor..next);
            }
        }

        // Left arrow
        (_, KeyCode::Left) => {
            if app.composer_cursor > 0 {
                app.composer_cursor = app.composer_text[..app.composer_cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
            }
        }

        // Right arrow
        (_, KeyCode::Right) => {
            if app.composer_cursor < app.composer_text.len() {
                app.composer_cursor = app.composer_text[app.composer_cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| app.composer_cursor + i)
                    .unwrap_or(app.composer_text.len());
            }
        }

        // Home
        (_, KeyCode::Home) => {
            app.composer_cursor = 0;
        }

        // End
        (_, KeyCode::End) => {
            app.composer_cursor = app.composer_text.len();
        }

        // Escape
        (_, KeyCode::Esc) => {
            app.slash_suppressed = false;
        }

        // Regular character
        (_, KeyCode::Char(ch)) => {
            insert_text_at_cursor(app, &ch.to_string());

            if app.composer_text.starts_with('/') && !app.slash_suppressed {
                app.slash_popup_active = true;
                app.slash_popup_filter = app.composer_text.clone();
                app.slash_popup_selected = 0;
            }
        }

        _ => {}
    }
    Ok(())
}

fn handle_slash_popup_key(
    app: &mut App,
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    key: KeyEvent,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Result<()> {
    let filtered = filtered_slash_commands(&app.composer_text);

    // If no slash commands match the current filter, close the popup
    // and fall through to normal key handling.
    if filtered.is_empty() {
        app.slash_popup_active = false;
        app.slash_suppressed = true;
        // Re-process this key through the normal handler
        return handle_key_event(app, term, key, agent_tx);
    }
    #[allow(clippy::collapsible_match)]
    match key.code {
        KeyCode::Esc => {
            app.slash_popup_active = false;
            app.slash_suppressed = true;
        }
        KeyCode::Up => {
            if app.slash_popup_selected > 0 {
                app.slash_popup_selected -= 1;
            }
        }
        KeyCode::Down => {
            if app.slash_popup_selected + 1 < filtered.len() {
                app.slash_popup_selected += 1;
            }
        }
        KeyCode::Enter => {
            if let Some(cmd) = filtered.get(app.slash_popup_selected) {
                app.composer_text = cmd.name.to_string();
                app.composer_cursor = app.composer_text.len();
            }
            app.slash_popup_active = false;
            app.slash_suppressed = true;
        }
        KeyCode::Tab => {
            if let Some(cmd) = filtered.first() {
                app.composer_text = cmd.name.to_string();
                app.composer_cursor = app.composer_text.len();
            }
        }
        KeyCode::Backspace => {
            if app.composer_cursor > 0 {
                let prev = app.composer_text[..app.composer_cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                app.composer_text.drain(prev..app.composer_cursor);
                app.composer_cursor = prev;
                if !app.composer_text.starts_with('/') {
                    app.slash_popup_active = false;
                }
            }
        }
        KeyCode::Char(ch) => {
            insert_text_at_cursor(app, &ch.to_string());
            if !app.composer_text.starts_with('/') {
                app.slash_popup_active = false;
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_transcript_event(app: &mut App, event: Event) -> Result<()> {
    if let Event::Key(key) = &event {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.transcript_active = false;
            }
            _ => {}
        }
    }
    Ok(())
}

fn handle_slash_command(
    app: &mut App,
    text: &str,
    _agent_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Result<()> {
    app.composer_text.clear();
    app.composer_cursor = 0;

    match text {
        "/exit" | "/quit" => {
            app.should_exit = true;
        }
        "/clear" => {
            app.pending_history.clear();
            app.streaming_text.clear();
            app.streaming_finalized = 0;
        }
        "/help" => {
            let help_lines: Vec<Line<'static>> = SLASH_COMMANDS
                .iter()
                .map(|cmd| {
                    Line::from(vec![
                        Span::styled(
                            format!("  {:<12}", cmd.name),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::raw(cmd.description),
                    ])
                })
                .collect();
            app.pending_history.extend(help_lines);
        }
        "/find" => {
            app.pending_history.push(Line::from(Span::styled(
                "  skill search not yet implemented in rebuilt TUI",
                Style::default().fg(Color::Yellow),
            )));
        }
        "/compact" => {
            app.pending_history.push(Line::from(Span::styled(
                "  compaction not yet implemented in rebuilt TUI",
                Style::default().fg(Color::Yellow),
            )));
        }
        _ => {
            app.pending_history.push(Line::from(Span::styled(
                format!("  unknown command: {text}"),
                Style::default().fg(Color::Red),
            )));
        }
    }
    Ok(())
}

// ── Agent event handling ──────────────────────────────────────────

fn handle_agent_event(app: &mut App, event: AgentEvent) -> Result<()> {
    match event {
        AgentEvent::UserMessage { .. } => {
            // User message is rendered at submit time in handle_key_event
            // to show skill chips correctly.
        }

        AgentEvent::AssistantMessageDelta { text } => {
            app.streaming_text.push_str(&text);
        }

        AgentEvent::AssistantMessageDone { .. } => {
            finalize_streaming(app);
            app.streaming_text.clear();
            app.streaming_finalized = 0;
        }

        AgentEvent::ToolCallStarted {
            name, arguments, ..
        } => {
            let args_preview =
                truncate_str(&serde_json::to_string(&arguments).unwrap_or_default(), 60);
            app.pending_history.push(Line::from(vec![
                Span::styled("  ⚙ ", Style::default().fg(Color::Blue)),
                Span::styled(
                    name.clone(),
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {args_preview}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }

        AgentEvent::ToolCallOutput {
            output, is_error, ..
        } => {
            let color = if is_error {
                Color::Red
            } else {
                Color::DarkGray
            };
            for line in output.lines().take(8) {
                app.pending_history.push(Line::from(Span::styled(
                    format!("    {line}"),
                    Style::default().fg(color),
                )));
            }
            let remaining = output.lines().count().saturating_sub(8);
            if remaining > 0 {
                app.pending_history.push(Line::from(Span::styled(
                    format!("    ... ({remaining} more lines)"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        AgentEvent::ToolCallApprovalRequest {
            tool,
            command,
            approval_id,
            ..
        } => {
            let desc = command.map(|c| c.join(" ")).unwrap_or_else(|| tool.clone());
            app.pending_history.push(Line::from(vec![
                Span::styled(
                    "  ⚠ approval required: ",
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    desc.clone(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            // Auto-deny for now - interactive approval will be wired later
            app.pending_approvals
                .respond(&approval_id, ReviewDecision::Denied);
        }

        AgentEvent::TurnComplete { usage } => {
            let elapsed = match &app.status_state {
                StatusState::Working { started } => started.elapsed(),
                StatusState::Ready => Duration::ZERO,
            };
            app.active_turn = None;
            app.status_state = StatusState::Ready;

            // Turn separator: usage + timing
            let summary = format_usage(&usage);
            let timing = format_elapsed(elapsed);

            // Separator line: ─── ↓1.2k ↑3.4k ─── 1.2s ───
            let sep_parts = if timing.is_empty() {
                format!("  {summary}")
            } else {
                format!("  \u{2500} {summary} \u{2500} {timing}")
            };
            app.pending_history.push(Line::from(Span::styled(
                sep_parts,
                Style::default().fg(Color::DarkGray),
            )));
            app.pending_history.push(Line::raw(""));
        }

        AgentEvent::TurnAborted { reason, .. } => {
            app.active_turn = None;
            app.status_state = StatusState::Ready;
            app.pending_history.push(Line::from(Span::styled(
                format!("  aborted: {reason}"),
                Style::default().fg(Color::Yellow),
            )));
            app.pending_history.push(Line::raw(""));
        }

        AgentEvent::Error { message } => {
            app.pending_history.push(Line::from(Span::styled(
                format!("  error: {message}"),
                Style::default().fg(Color::Red),
            )));
        }

        AgentEvent::ProviderRetry {
            attempt,
            max_attempts,
            delay_ms,
            reason,
        } => {
            app.pending_history.push(Line::from(Span::styled(
                format!("  retry {attempt}/{max_attempts} in {delay_ms}ms: {reason}"),
                Style::default().fg(Color::Yellow),
            )));
        }

        _ => {}
    }
    Ok(())
}

// ── Prompt submission ─────────────────────────────────────────────

fn submit_prompt(
    app: &mut App,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
    prompt: &str,
) -> Result<()> {
    let bypass = app.args.dangerously_bypass_approvals_and_sandbox;
    let policy = if bypass {
        AskForApproval::OnRequest
    } else {
        app.args.approval_policy
    };

    let sandbox_policy = sandbox_policy_from_args(&app.args, &app.cwd);
    let sandbox = Arc::from(select_for_platform(&sandbox_policy));

    let gate: Arc<dyn nav_core::guardrails::approval::ApprovalGate> = if bypass {
        Arc::new(AutoGate::approving())
    } else if matches!(policy, AskForApproval::Never) {
        Arc::new(AutoGate::denying())
    } else {
        Arc::new(ChannelGate::new(
            app.pending_approvals.clone(),
            agent_tx.clone(),
        ))
    };

    let permissions = PermissionContext {
        gate,
        policy,
        sandbox_policy,
        sandbox,
        session_allowlist: SessionAllowlist::default(),
    };

    let transport = app.transport.clone();
    let args = app.args.clone();
    let cwd = app.cwd.clone();
    let skills = Arc::clone(&app.skills);
    let extensions = Arc::clone(&app.extensions);
    let project = Arc::clone(&app.project);
    let prompt_owned = prompt.to_string();
    let session_id = app.session_id.clone();
    let store = Arc::clone(&app.store);

    let handle = tokio::spawn(async move {
        let binding = SessionBinding {
            store: store.as_ref(),
            session_id,
        };
        let request = AgentTurnRequest::new(
            &transport,
            &args,
            &cwd,
            &prompt_owned,
            agent_tx,
            skills.as_ref(),
            permissions,
        )
        .with_session(Some(&binding), None)
        .with_extensions(Some(extensions.as_ref()))
        .with_context(Some(project.as_ref()));

        let _ = run_agent(request).await;
    });

    app.active_turn = Some(ActiveTurn { _handle: handle });
    app.status_state = StatusState::Working {
        started: Instant::now(),
    };

    Ok(())
}

// ── Streaming finalization ────────────────────────────────────────

fn finalize_streaming(app: &mut App) {
    let remaining = &app.streaming_text[app.streaming_finalized..];
    if !remaining.is_empty() {
        for line in remaining.split('\n') {
            app.pending_history.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::White),
            )));
        }
        app.streaming_finalized = app.streaming_text.len();
    }
}

// ── Helpers ───────────────────────────────────────────────────────

fn insert_text_at_cursor(app: &mut App, text: &str) {
    app.composer_text.insert_str(app.composer_cursor, text);
    app.composer_cursor += text.len();
}

fn filtered_slash_commands(filter: &str) -> Vec<&'static SlashCommand> {
    SLASH_COMMANDS
        .iter()
        .filter(|cmd| cmd.name.starts_with(filter))
        .collect()
}

fn render_startup_notices_to_history(app: &mut App, notices: &StartupNotices) {
    for notice in notices.iter() {
        let color = match notice.level {
            nav_core::NoticeLevel::Warning => Color::Yellow,
            nav_core::NoticeLevel::Error => Color::Red,
        };
        app.pending_history.push(Line::from(Span::styled(
            format!("  ⚠ {}", notice.message),
            Style::default().fg(color),
        )));
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let cut = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= max)
            .last()
            .unwrap_or(0);
        format!("{}...", &s[..cut])
    }
}

fn format_usage(usage: &TurnUsage) -> String {
    let input = compact_count(usage.tokens_input);
    let output = compact_count(usage.tokens_output);
    if usage.tokens_input > 0 || usage.tokens_output > 0 {
        format!(
            "\u{2193}{input} \u{2191}{output}",
            input = input,
            output = output
        )
    } else {
        "turn complete".to_string()
    }
}

fn compact_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_elapsed(d: Duration) -> String {
    let ms = d.as_millis();
    if ms == 0 {
        return String::new();
    }
    if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}
