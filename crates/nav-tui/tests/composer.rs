//! End-to-end tests for the bottom-pane composer driven through
//! [`ratatui::backend::TestBackend`] with simulated [`KeyEvent`]s.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nav_core::{AgentEvent, PendingInputMode};
use nav_tui::bottom_pane::{BottomPane, ComposerEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn press(pane: &mut BottomPane, code: KeyCode, mods: KeyModifiers) -> ComposerEvent {
    pane.handle_key(KeyEvent::new(code, mods))
}

fn type_text(pane: &mut BottomPane, text: &str) {
    for c in text.chars() {
        press(pane, KeyCode::Char(c), KeyModifiers::NONE);
    }
}

fn fresh_terminal() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(40, 8)).expect("terminal")
}

fn render(pane: &BottomPane, terminal: &mut Terminal<TestBackend>) {
    terminal
        .draw(|frame| {
            let area = frame.area();
            frame.render_widget(pane, area);
        })
        .expect("draw");
}

fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
    let buf = terminal.backend().buffer();
    let area = buf.area();
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn typing_hello_then_enter_returns_submit() {
    let mut pane = BottomPane::new();
    let mut terminal = fresh_terminal();

    type_text(&mut pane, "hello");
    render(&pane, &mut terminal);
    assert_eq!(pane.composer().text(), "hello");

    let event = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        event,
        ComposerEvent::Submit {
            text: "hello".to_string(),
            attachments: Vec::new(),
        }
    );
    assert_eq!(pane.composer().text(), "");
    assert_eq!(pane.composer().history(), &["hello".to_string()]);
}

#[test]
fn slash_shows_popup_and_he_filters_to_help() {
    let mut pane = BottomPane::new();
    let mut terminal = fresh_terminal();

    press(&mut pane, KeyCode::Char('/'), KeyModifiers::NONE);
    assert!(pane.has_overlay(), "popup must appear on the leading slash");
    let popup = pane.slash_popup().expect("slash popup");
    assert_eq!(popup.filter(), "/");
    let commands: Vec<&str> = popup
        .matches()
        .iter()
        .map(|entry| entry.command.as_str())
        .collect();
    assert_eq!(
        commands,
        vec![
            "/help",
            "/clear",
            "/quit",
            "/exit",
            "/resume",
            "/sessions",
            "/name",
            "/export",
            "/fork",
            "/tree",
            "/label",
            "/unlabel",
            "/find",
            "/compact",
            "/abort",
            "/steer",
            "/queue-edit",
            "/queue-remove",
            "/queue-clear",
        ]
    );

    type_text(&mut pane, "he");
    render(&pane, &mut terminal);

    let popup = pane.slash_popup().expect("slash popup remains");
    assert_eq!(popup.filter(), "/he");
    let commands: Vec<&str> = popup
        .matches()
        .iter()
        .map(|entry| entry.command.as_str())
        .collect();
    assert_eq!(commands, vec!["/help"]);
}

#[test]
fn exact_slash_command_enter_submits_without_second_enter() {
    let mut pane = BottomPane::new();

    type_text(&mut pane, "/exit");
    let event = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        event,
        ComposerEvent::Submit {
            text: "/exit".to_string(),
            attachments: Vec::new(),
        }
    );
    assert_eq!(pane.composer().text(), "");
    assert_eq!(pane.composer().history(), &["/exit".to_string()]);
}

#[test]
fn partial_slash_command_enter_completes_and_submits() {
    let mut pane = BottomPane::new();

    type_text(&mut pane, "/ex");
    let event = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);

    assert_eq!(
        event,
        ComposerEvent::Submit {
            text: "/exit".to_string(),
            attachments: Vec::new(),
        }
    );
    assert_eq!(pane.composer().text(), "");
    assert_eq!(pane.composer().history(), &["/exit".to_string()]);
}

#[test]
fn slash_popup_lists_catalog_skills() {
    use nav_core::{Catalog, Skill, SkillScope};
    use nav_tui::bottom_pane::build_slash_entries;
    let catalog = Catalog::new(vec![Skill {
        name: "foo".into(),
        description: "do foo".into(),
        skill_md_path: "/tmp/foo/SKILL.md".into(),
        skill_dir: "/tmp/foo".into(),
        scope: SkillScope::Project,
    }]);
    let mut pane = BottomPane::with_slash_entries(build_slash_entries(&catalog));
    press(&mut pane, KeyCode::Char('/'), KeyModifiers::NONE);
    let popup = pane.slash_popup().expect("slash popup");
    let commands: Vec<&str> = popup
        .matches()
        .iter()
        .map(|entry| entry.command.as_str())
        .collect();
    assert!(
        commands.contains(&"/foo"),
        "catalog skill missing: {commands:?}"
    );
    assert!(commands.contains(&"/help"));
}

#[test]
fn shift_enter_inserts_newline_and_does_not_submit() {
    let mut pane = BottomPane::new();
    let mut terminal = fresh_terminal();

    press(&mut pane, KeyCode::Char('a'), KeyModifiers::NONE);
    let event = press(&mut pane, KeyCode::Enter, KeyModifiers::SHIFT);
    assert_eq!(event, ComposerEvent::Nothing);
    press(&mut pane, KeyCode::Char('b'), KeyModifiers::NONE);
    render(&pane, &mut terminal);

    assert_eq!(pane.composer().text(), "a\nb");
    assert!(pane.composer().history().is_empty());
}

#[test]
fn up_arrow_recalls_previous_prompt() {
    let mut pane = BottomPane::new();
    let mut terminal = fresh_terminal();

    type_text(&mut pane, "first");
    let event = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        event,
        ComposerEvent::Submit {
            text: "first".to_string(),
            attachments: Vec::new(),
        }
    );
    assert_eq!(pane.composer().text(), "");

    press(&mut pane, KeyCode::Up, KeyModifiers::NONE);
    render(&pane, &mut terminal);
    assert_eq!(pane.composer().text(), "first");
}

#[test]
fn bottom_pane_renders_pending_followups_and_steering_above_composer() {
    let mut pane = BottomPane::new();
    let mut terminal = Terminal::new(TestBackend::new(80, 10)).expect("terminal");

    pane.apply_agent_event(&AgentEvent::PendingInputQueued {
        id: "pending-1".into(),
        mode: PendingInputMode::FollowUp,
        text: "run tests next".into(),
        display_text: None,
        attachments: Vec::new(),
        skill_name: None,
    });
    pane.apply_agent_event(&AgentEvent::PendingInputQueued {
        id: "pending-2".into(),
        mode: PendingInputMode::Steering,
        text: "avoid broad refactors".into(),
        display_text: None,
        attachments: Vec::new(),
        skill_name: None,
    });

    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);

    assert!(rendered.contains("pending"), "{rendered}");
    assert!(rendered.contains("pending-1 follow-up"), "{rendered}");
    assert!(rendered.contains("run tests next"), "{rendered}");
    assert!(rendered.contains("pending-2 steering"), "{rendered}");
    assert!(rendered.contains("avoid broad refactors"), "{rendered}");
}

#[test]
fn bottom_pane_updates_pending_preview_for_edit_remove_and_clear() {
    let mut pane = BottomPane::new();
    let mut terminal = Terminal::new(TestBackend::new(80, 10)).expect("terminal");

    pane.apply_agent_event(&AgentEvent::PendingInputQueued {
        id: "pending-1".into(),
        mode: PendingInputMode::FollowUp,
        text: "first wording".into(),
        display_text: None,
        attachments: Vec::new(),
        skill_name: None,
    });
    pane.apply_agent_event(&AgentEvent::PendingInputQueued {
        id: "pending-2".into(),
        mode: PendingInputMode::Steering,
        text: "steer this".into(),
        display_text: None,
        attachments: Vec::new(),
        skill_name: None,
    });
    pane.apply_agent_event(&AgentEvent::PendingInputEdited {
        id: "pending-1".into(),
        text: "better wording".into(),
        display_text: None,
        attachments: Vec::new(),
        skill_name: None,
    });
    pane.apply_agent_event(&AgentEvent::PendingInputRemoved {
        id: "pending-2".into(),
    });

    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);
    assert!(rendered.contains("pending-1 follow-up"), "{rendered}");
    assert!(rendered.contains("better wording"), "{rendered}");
    assert!(!rendered.contains("pending-2"), "{rendered}");

    pane.apply_agent_event(&AgentEvent::PendingInputCleared {
        ids: vec!["pending-1".into()],
    });
    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);
    assert!(!rendered.contains("pending-1"), "{rendered}");
}
