use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nav_core::{Catalog, PendingInputMode, PendingSkill, UserAttachment};
use tokio::sync::mpsc;

use crate::ChatWidget;

#[derive(Debug)]
pub(crate) enum AppEvent {
    Submit {
        text: String,
        display_text: Option<String>,
        attachments: Vec<UserAttachment>,
        mode: PendingInputMode,
        skill: Option<PendingSkill>,
    },
    Quit,
    Clear,
    AbortTurn,
    EditPending {
        id: String,
        text: String,
    },
    RemovePending {
        id: String,
    },
    ClearPending,
    /// Standalone `/<skill>` - the wrapped body is held until the next
    /// non-slash prompt rather than fired as its own turn.
    QueueSkill {
        skill: PendingSkill,
    },
    ListSessions,
    Resume {
        query: Option<String>,
    },
    NameSession {
        name: String,
    },
    Export {
        path: Option<PathBuf>,
    },
    ForkSession {
        at: Option<u64>,
    },
    ShowTree,
    AddLabel {
        label: String,
    },
    RemoveLabel {
        label: String,
    },
    FindTranscript {
        query: String,
    },
    SlashError {
        message: String,
    },
}

pub(crate) fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

pub(crate) fn handle_scrollback_key(
    chat: &mut ChatWidget,
    key: &KeyEvent,
    (history_w, history_h): (u16, u16),
    allow_line_scroll: bool,
) -> bool {
    const LINE_SCROLL_ROWS: u16 = 3;

    let page_rows = history_h.saturating_sub(1).max(1);
    match key.code {
        KeyCode::Up if allow_line_scroll => chat.scroll_up(LINE_SCROLL_ROWS, history_w, history_h),
        KeyCode::Down if allow_line_scroll => {
            chat.scroll_down(LINE_SCROLL_ROWS, history_w, history_h)
        }
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

pub(crate) fn dispatch_submit(
    text: String,
    attachments: Vec<UserAttachment>,
    skills: &Catalog,
    app_tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let event = match parse_builtin_command(&text) {
        Some(event) => event,
        None => submit_event_for_text(text, attachments, skills),
    };
    app_tx.send(event).ok();
}

fn submit_event_for_text(
    text: String,
    attachments: Vec<UserAttachment>,
    skills: &Catalog,
) -> AppEvent {
    match text.as_str() {
        "/quit" | "/exit" => AppEvent::Quit,
        "/clear" => AppEvent::Clear,
        "/abort" => AppEvent::AbortTurn,
        "/queue-clear" => AppEvent::ClearPending,
        // `/compact` is handled inside nav-core's `run_agent` — submit the
        // literal text so the agent loop's `is_compact_command` check
        // dispatches the non-steerable compaction turn.
        "/compact" => submit_event(text, None, attachments, PendingInputMode::FollowUp, None),
        _ => skill_or_submit_event(text, attachments, skills),
    }
}

fn skill_or_submit_event(
    text: String,
    attachments: Vec<UserAttachment>,
    skills: &Catalog,
) -> AppEvent {
    match classify_slash(&text, skills) {
        SlashAction::Control(control) => control.into_event(attachments),
        SlashAction::NotASkill => {
            submit_event(text, None, attachments, PendingInputMode::FollowUp, None)
        }
        SlashAction::Inline {
            skill_name,
            wrapped_body,
            request,
        } => submit_event(
            request.clone(),
            Some(request),
            attachments,
            PendingInputMode::FollowUp,
            Some(PendingSkill {
                name: skill_name,
                wrapped_body,
            }),
        ),
        SlashAction::Queue {
            skill_name,
            wrapped_body,
        } => AppEvent::QueueSkill {
            skill: PendingSkill {
                name: skill_name,
                wrapped_body,
            },
        },
    }
}

fn parse_builtin_command(text: &str) -> Option<AppEvent> {
    let trimmed = text.trim();
    if trimmed == "/sessions" {
        return Some(AppEvent::ListSessions);
    }
    if let Some(rest) = slash_rest(trimmed, "/resume") {
        return Some(AppEvent::Resume {
            query: (!rest.is_empty()).then(|| rest.to_string()),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/name") {
        if rest.is_empty() {
            return Some(AppEvent::SlashError {
                message: "usage: /name <text>".to_string(),
            });
        }
        return Some(AppEvent::NameSession {
            name: rest.to_string(),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/export") {
        return Some(AppEvent::Export {
            path: (!rest.is_empty()).then(|| PathBuf::from(rest)),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/fork") {
        let at = if rest.is_empty() {
            None
        } else {
            match rest.parse::<u64>() {
                Ok(seq) => Some(seq),
                Err(_) => {
                    return Some(AppEvent::SlashError {
                        message: format!("usage: /fork [seq]  (got {rest:?})"),
                    });
                }
            }
        };
        return Some(AppEvent::ForkSession { at });
    }
    if trimmed == "/tree" {
        return Some(AppEvent::ShowTree);
    }
    if let Some(rest) = slash_rest(trimmed, "/label") {
        if rest.is_empty() {
            return Some(AppEvent::SlashError {
                message: "usage: /label <text>".to_string(),
            });
        }
        return Some(AppEvent::AddLabel {
            label: rest.to_string(),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/unlabel") {
        if rest.is_empty() {
            return Some(AppEvent::SlashError {
                message: "usage: /unlabel <text>".to_string(),
            });
        }
        return Some(AppEvent::RemoveLabel {
            label: rest.to_string(),
        });
    }
    if let Some(rest) = slash_rest(trimmed, "/find") {
        if rest.is_empty() {
            return Some(AppEvent::SlashError {
                message: "usage: /find <query>".to_string(),
            });
        }
        return Some(AppEvent::FindTranscript {
            query: rest.to_string(),
        });
    }
    None
}

fn slash_rest<'a>(text: &'a str, command: &str) -> Option<&'a str> {
    if text == command {
        return Some("");
    }
    text.strip_prefix(command)
        .and_then(|rest| rest.strip_prefix(char::is_whitespace))
        .map(str::trim)
}

fn submit_event(
    text: String,
    display_text: Option<String>,
    attachments: Vec<UserAttachment>,
    mode: PendingInputMode,
    skill: Option<PendingSkill>,
) -> AppEvent {
    AppEvent::Submit {
        text,
        display_text,
        attachments,
        mode,
        skill,
    }
}

/// Classification of a submitted composer line that may be a skill activation.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashAction {
    NotASkill,
    Control(ControlCommand),
    /// Standalone `/<skill-name>`. The wrapped body should be queued and
    /// prepended to the next real prompt - sending it as its own turn would
    /// be lost, since each `run_agent` call replays no prior history.
    Queue {
        skill_name: String,
        wrapped_body: String,
    },
    /// `/<skill-name> <request>` - wrap and request travel together.
    Inline {
        skill_name: String,
        wrapped_body: String,
        request: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ControlCommand {
    Steer { text: String },
    EditPending { id: String, text: String },
    RemovePending { id: String },
    ClearPending,
    AbortTurn,
}

impl ControlCommand {
    fn into_event(self, attachments: Vec<UserAttachment>) -> AppEvent {
        match self {
            ControlCommand::Steer { text } => {
                submit_event(text, None, attachments, PendingInputMode::Steering, None)
            }
            ControlCommand::EditPending { id, text } => AppEvent::EditPending { id, text },
            ControlCommand::RemovePending { id } => AppEvent::RemovePending { id },
            ControlCommand::ClearPending => AppEvent::ClearPending,
            ControlCommand::AbortTurn => AppEvent::AbortTurn,
        }
    }
}

/// Wraps the leading `/<skill-name>` (if any) in a `<skill name=... dir=...>`
/// block so the model can load instructions and resolve relative resources
/// against the skill's directory. Scripts/references inside the SKILL.md
/// are not read here - the model loads them on demand.
pub fn classify_slash(text: &str, skills: &Catalog) -> SlashAction {
    let trimmed = text.trim_start();
    let Some(first_token) = trimmed.split_whitespace().next() else {
        return SlashAction::NotASkill;
    };
    if let Some(control) = classify_control_command(trimmed, first_token) {
        return SlashAction::Control(control);
    }
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
            skill_name: skill.name.clone(),
            wrapped_body,
            request: rest.to_string(),
        }
    }
}

fn classify_control_command(trimmed: &str, first_token: &str) -> Option<ControlCommand> {
    let rest = trimmed[first_token.len()..].trim_start();
    match first_token {
        "/abort" if rest.is_empty() => Some(ControlCommand::AbortTurn),
        "/queue-clear" if rest.is_empty() => Some(ControlCommand::ClearPending),
        "/steer" if !rest.is_empty() => Some(ControlCommand::Steer {
            text: rest.to_string(),
        }),
        "/queue-remove" => (!rest.is_empty()).then(|| ControlCommand::RemovePending {
            id: rest.to_string(),
        }),
        "/queue-edit" => {
            let (id, text) = rest.split_once(char::is_whitespace)?;
            let text = text.trim_start();
            (!id.is_empty() && !text.is_empty()).then(|| ControlCommand::EditPending {
                id: id.to_string(),
                text: text.to_string(),
            })
        }
        _ => None,
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
    use nav_core::{AgentEvent, Catalog, Skill, SkillScope};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use std::fs;
    use tempfile::tempdir;

    fn buffer_to_text(buf: &Buffer) -> String {
        let area = buf.area();
        let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
        for y in 0..area.height {
            let row_start = out.len();
            for x in 0..area.width {
                out.push_str(buf[(area.x + x, area.y + y)].symbol());
            }
            let trimmed = out[row_start..].trim_end_matches(' ').len();
            out.truncate(row_start + trimmed);
            out.push('\n');
        }
        out
    }

    fn render_widget(widget: &ChatWidget, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(widget, area);
            })
            .expect("draw");
        buffer_to_text(terminal.backend().buffer())
    }

    fn widget_with_numbered_output(line_count: usize) -> ChatWidget {
        let mut widget = ChatWidget::new();
        widget.ingest(AgentEvent::ToolCallOutput {
            call_id: "call_1".to_string(),
            output: (0..line_count)
                .map(|i| format!("line {i:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
            is_error: false,
        });
        widget
    }

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
            SlashAction::Inline {
                skill_name,
                wrapped_body,
                request,
            } => {
                assert_eq!(skill_name, "foo");
                assert!(wrapped_body.contains("</skill>"));
                assert_eq!(request, "please help with X");
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

    #[test]
    fn page_keys_scroll_scrollback() {
        let mut widget = widget_with_numbered_output(20);

        assert!(handle_scrollback_key(
            &mut widget,
            &KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            (40, 6),
            false,
        ));
        let older = render_widget(&widget, 40, 6);
        assert!(!older.contains("line 19"), "{older}");

        assert!(handle_scrollback_key(
            &mut widget,
            &KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            (40, 6),
            false,
        ));
        let newest = render_widget(&widget, 40, 6);
        assert!(newest.contains("line 19"), "{newest}");
    }

    #[test]
    fn non_scrollback_keys_do_not_steal_composer_input() {
        let mut widget = widget_with_numbered_output(20);

        assert!(!handle_scrollback_key(
            &mut widget,
            &KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            (40, 6),
            true,
        ));
        let rendered = render_widget(&widget, 40, 6);

        assert!(rendered.contains("line 19"), "{rendered}");
    }

    #[test]
    fn line_scroll_keys_only_scroll_when_enabled() {
        let mut widget = widget_with_numbered_output(20);

        assert!(!handle_scrollback_key(
            &mut widget,
            &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            (40, 6),
            false,
        ));
        let newest = render_widget(&widget, 40, 6);
        assert!(newest.contains("line 19"), "{newest}");

        assert!(handle_scrollback_key(
            &mut widget,
            &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            (40, 6),
            true,
        ));
        let older = render_widget(&widget, 40, 6);
        assert!(!older.contains("line 19"), "{older}");
    }

    #[test]
    fn dispatch_submit_routes_compact_through_submit_path() {
        // `/compact` is implemented inside nav-core, not as a local TUI
        // gesture. dispatch_submit must forward the literal text so the
        // agent loop's is_compact_command check fires.
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
        dispatch_submit("/compact".to_string(), Vec::new(), &catalog, &tx);
        let event = rx.try_recv().expect("event sent");
        match event {
            AppEvent::Submit {
                text, attachments, ..
            } => {
                assert_eq!(text, "/compact");
                assert!(attachments.is_empty());
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_submit_routes_session_management_commands_locally() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

        dispatch_submit("/sessions".to_string(), Vec::new(), &catalog, &tx);
        assert!(matches!(rx.try_recv().unwrap(), AppEvent::ListSessions));

        dispatch_submit("/resume".to_string(), Vec::new(), &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::Resume { query: None }
        ));

        dispatch_submit("/resume 01HZ".to_string(), Vec::new(), &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::Resume { query: Some(q) } if q == "01HZ"
        ));

        dispatch_submit("/name release work".to_string(), Vec::new(), &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::NameSession { name } if name == "release work"
        ));

        dispatch_submit(
            "/export transcript.md".to_string(),
            Vec::new(),
            &catalog,
            &tx,
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::Export { path: Some(path) } if path.as_path() == std::path::Path::new("transcript.md")
        ));
    }

    #[test]
    fn dispatch_submit_reports_missing_name_argument() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

        dispatch_submit("/name".to_string(), Vec::new(), &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::SlashError { message } if message.contains("/name")
        ));
    }

    #[test]
    fn dispatch_submit_routes_control_commands_locally() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

        dispatch_submit(
            "/steer add this context".to_string(),
            Vec::new(),
            &catalog,
            &tx,
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::Submit {
                text,
                mode: nav_core::PendingInputMode::Steering,
                ..
            } if text == "add this context"
        ));

        dispatch_submit(
            "/queue-edit pending-1 better wording".to_string(),
            Vec::new(),
            &catalog,
            &tx,
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::EditPending { id, text } if id == "pending-1" && text == "better wording"
        ));

        dispatch_submit(
            "/queue-remove pending-1".to_string(),
            Vec::new(),
            &catalog,
            &tx,
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::RemovePending { id } if id == "pending-1"
        ));

        dispatch_submit("/queue-clear".to_string(), Vec::new(), &catalog, &tx);
        assert!(matches!(rx.try_recv().unwrap(), AppEvent::ClearPending));

        dispatch_submit("/abort".to_string(), Vec::new(), &catalog, &tx);
        assert!(matches!(rx.try_recv().unwrap(), AppEvent::AbortTurn));
    }
}
