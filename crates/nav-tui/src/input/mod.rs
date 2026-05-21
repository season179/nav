//! User input helpers that sit outside the composer widget itself.
//!
//! The bottom pane owns raw text editing. This module handles commands that
//! affect the whole app, such as `/resume`, `/context`, and transcript
//! scrollback keys.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ChatWidget;

mod commands;
mod slash;

#[cfg(test)]
use commands::parse_builtin_command;
pub(crate) use commands::{AppEvent, dispatch_submit};
#[cfg(test)]
use slash::classify_slash_with_extensions;
pub use slash::{SlashAction, classify_slash, prepend_pending_skill};

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

#[cfg(test)]
mod tests {
    use super::*;
    use nav_core::{
        AgentEvent, Catalog, ExtensionCatalog, ExtensionScope, PromptTemplate, Skill, SkillScope,
    };
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
            truncation: None,
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

    fn extensions_with_template(dir: &std::path::Path) -> ExtensionCatalog {
        let extension_dir = dir.join("demo-extension");
        fs::create_dir_all(&extension_dir).unwrap();
        let body_path = extension_dir.join("review.md");
        fs::write(&body_path, "Review the change carefully.\n").unwrap();
        ExtensionCatalog::new(
            Vec::new(),
            vec![PromptTemplate {
                name: "review".into(),
                description: "review changes".into(),
                body_path,
                extension_name: "demo".into(),
                extension_dir,
                scope: ExtensionScope::Project,
            }],
            Vec::new(),
        )
    }

    fn dispatch(text: &str, catalog: &Catalog, tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>) {
        dispatch_submit(
            text.to_string(),
            Vec::new(),
            catalog,
            &ExtensionCatalog::default(),
            tx,
        );
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
    fn parses_git_builtin_commands() {
        assert!(matches!(
            parse_builtin_command("/checkpoint before risky edit"),
            Some(AppEvent::GitCheckpoint { label: Some(label) }) if label == "before risky edit"
        ));
        assert!(matches!(
            parse_builtin_command("/stash"),
            Some(AppEvent::GitStash { label: None })
        ));
        assert!(matches!(
            parse_builtin_command("/restore stash@{2}"),
            Some(AppEvent::GitRestore { target: Some(target) }) if target == "stash@{2}"
        ));
    }

    #[test]
    fn classify_slash_queues_prompt_template() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        let extensions = extensions_with_template(dir.path());

        match classify_slash_with_extensions("/prompt:review", &catalog, &extensions) {
            SlashAction::Queue {
                skill_name,
                wrapped_body,
            } => {
                assert_eq!(skill_name, "prompt:review");
                assert!(wrapped_body.contains("<prompt_template name=\"review\""));
                assert!(wrapped_body.contains("Review the change carefully."));
                assert!(wrapped_body.trim_end().ends_with("</prompt_template>"));
            }
            other => panic!("expected Queue, got {other:?}"),
        }
    }

    #[test]
    fn classify_slash_inlines_prompt_template_with_request() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        let extensions = extensions_with_template(dir.path());

        match classify_slash_with_extensions(
            "/prompt:review focus on regressions",
            &catalog,
            &extensions,
        ) {
            SlashAction::Inline {
                skill_name,
                wrapped_body,
                request,
            } => {
                assert_eq!(skill_name, "prompt:review");
                assert!(wrapped_body.contains("</prompt_template>"));
                assert_eq!(request, "focus on regressions");
            }
            other => panic!("expected Inline, got {other:?}"),
        }
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
        dispatch("/compact", &catalog, &tx);
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

        dispatch("/sessions", &catalog, &tx);
        assert!(matches!(rx.try_recv().unwrap(), AppEvent::ListSessions));

        dispatch("/resume", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::Resume { query: None }
        ));

        dispatch("/resume 01HZ", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::Resume { query: Some(q) } if q == "01HZ"
        ));

        dispatch("/name release work", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::NameSession { name } if name == "release work"
        ));

        dispatch("/export transcript.md", &catalog, &tx);
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

        dispatch("/name", &catalog, &tx);
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

        dispatch("/steer add this context", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::Submit {
                text,
                mode: nav_core::PendingInputMode::Steering,
                ..
            } if text == "add this context"
        ));

        dispatch("/queue-edit pending-1 better wording", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::EditPending { id, text } if id == "pending-1" && text == "better wording"
        ));

        dispatch("/queue-remove pending-1", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::RemovePending { id } if id == "pending-1"
        ));

        dispatch("/queue-clear", &catalog, &tx);
        assert!(matches!(rx.try_recv().unwrap(), AppEvent::ClearPending));

        dispatch("/abort", &catalog, &tx);
        assert!(matches!(rx.try_recv().unwrap(), AppEvent::AbortTurn));

        dispatch("/context", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::ShowContext { include_all: false }
        ));

        dispatch("/context all", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::ShowContext { include_all: true }
        ));

        dispatch("/handoff finish issue 54", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::Handoff { goal } if goal == "finish issue 54"
        ));
    }

    #[test]
    fn dispatch_submit_reports_missing_handoff_goal() {
        let dir = tempdir().unwrap();
        let catalog = catalog_with_skill(dir.path());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

        dispatch("/handoff", &catalog, &tx);
        assert!(matches!(
            rx.try_recv().unwrap(),
            AppEvent::SlashError { message } if message.contains("/handoff")
        ));
    }
}
