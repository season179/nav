//! End-to-end tests for the bottom-pane composer driven through
//! [`ratatui::backend::TestBackend`] with simulated [`KeyEvent`]s.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nav_core::{AgentEvent, PendingInputMode};
use nav_tui::bottom_pane::{
    AgentState, BottomPane, ComposerEvent, MentionEntry, SlashEntry, StatusBarState,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::PathBuf;
use std::sync::Arc;

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
    assert_eq!(commands, vec!["/exit", "/find", "/fork"]);

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
fn at_file_shows_mention_popup_and_filters_paths() {
    let mention_entries: Arc<[MentionEntry]> = vec![
        MentionEntry {
            display: "README.md".to_string(),
        },
        MentionEntry {
            display: "crates/nav-tui/src/bottom_pane/composer.rs".to_string(),
        },
    ]
    .into();
    let mut pane = BottomPane::with_entries(
        Arc::from(Vec::<SlashEntry>::new()),
        mention_entries,
        PathBuf::from("."),
    );
    let mut terminal = fresh_terminal();

    type_text(&mut pane, "@read");
    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);

    assert!(pane.has_overlay(), "mention popup did not open: {rendered}");
    assert!(rendered.contains("README.md"), "{rendered}");
}

#[test]
fn altgr_at_file_shows_mention_popup() {
    let mention_entries: Arc<[MentionEntry]> = vec![MentionEntry {
        display: "README.md".to_string(),
    }]
    .into();
    let mut pane = BottomPane::with_entries(
        Arc::from(Vec::<SlashEntry>::new()),
        mention_entries,
        PathBuf::from("."),
    );
    let mut terminal = fresh_terminal();

    press(
        &mut pane,
        KeyCode::Char('@'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    type_text(&mut pane, "read");
    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);

    assert_eq!(pane.composer().text(), "@read");
    assert!(pane.has_overlay(), "mention popup did not open: {rendered}");
    assert!(rendered.contains("README.md"), "{rendered}");
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
    type_text(&mut pane, "fo");
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
    assert!(commands.contains(&"/fork"));
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

fn ready_status() -> StatusBarState {
    StatusBarState {
        model: "test-model".into(),
        cwd_short: "~/proj".into(),
        branch: Some("main".into()),
        dirty: false,
        agent_state: AgentState::Ready,
        ..StatusBarState::default()
    }
}

#[test]
fn status_bar_renders_below_composer() {
    let mut pane = BottomPane::new();
    pane.update_status(ready_status());

    let mut terminal = Terminal::new(TestBackend::new(80, 6)).expect("terminal");
    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);

    // Layout order: composer first, status bar on the last row (matches
    // codex). The status bar separator (`  ·  `) followed by the
    // agent-state word is the canonical proof that the status row painted
    // at all.
    let lines: Vec<&str> = rendered.lines().collect();
    let status_line = lines
        .iter()
        .rposition(|line| line.contains("·  Ready"))
        .expect("status row should render");
    assert_eq!(
        status_line,
        lines.len() - 1,
        "status bar should occupy the bottom row of the pane:\n{rendered}"
    );
    assert!(
        lines[status_line].contains("test-model") && lines[status_line].contains("main"),
        "status row should carry model + branch:\n{rendered}"
    );

    // Composer prompt (`›`) must appear above the status row.
    let prompt_line = lines
        .iter()
        .position(|line| line.contains('›'))
        .expect("composer prompt should render somewhere");
    assert!(
        prompt_line < status_line,
        "composer prompt should sit above the status row (prompt={prompt_line}, status={status_line}):\n{rendered}"
    );
}

#[test]
fn desired_height_includes_status_row_and_composer_floor() {
    let pane = BottomPane::new();
    // Status row (1) + composer floor (3) = 4. The pane should never report
    // less than that; if it does, `draw_tui` will clip the status bar or the
    // composer padding.
    assert!(
        pane.desired_height(80) >= 4,
        "BottomPane minimum height must reserve the status row plus composer floor"
    );
}

fn working_status(show_indicator: bool) -> StatusBarState {
    StatusBarState {
        model: "test-model".into(),
        cwd_short: "~/proj".into(),
        branch: Some("main".into()),
        dirty: false,
        agent_state: AgentState::Working {
            elapsed: std::time::Duration::from_secs(7),
            spinner: '⠴',
            tick: 0,
        },
        show_indicator,
        ..StatusBarState::default()
    }
}

#[test]
fn working_indicator_row_renders_above_composer_with_status_on_bottom() {
    let mut pane = BottomPane::new();
    pane.update_status(working_status(true));

    let mut terminal = Terminal::new(TestBackend::new(80, 7)).expect("terminal");
    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);
    let lines: Vec<&str> = rendered.lines().collect();

    // Indicator row sits at the top of the pane (above the composer);
    // the status bar paints on the bottom row.
    let indicator_line = lines
        .iter()
        .position(|line| line.contains("Ctrl+C to interrupt"))
        .expect("indicator row should render somewhere");
    assert_eq!(
        indicator_line, 0,
        "indicator row should sit at the top of the pane, above the composer:\n{rendered}"
    );

    // When the indicator is visible the status bar must NOT also paint a
    // "Working Ns" segment — that would duplicate the busy signal. The
    // bottom row should still carry the rest of the bar (model + cwd +
    // branch); look for `test-model` to confirm it rendered.
    let status_line = lines
        .iter()
        .rposition(|line| line.contains("test-model"))
        .expect("status bar should still render on the bottom row");
    assert_eq!(
        status_line,
        lines.len() - 1,
        "status bar should occupy the bottom row of the pane:\n{rendered}"
    );
    assert!(
        !lines[status_line].contains("Working"),
        "status bar must drop the inline `Working Ns` segment when the indicator is visible:\n{rendered}"
    );

    let prompt_line = lines
        .iter()
        .position(|line| line.contains('›'))
        .expect("composer prompt should render");
    assert!(
        prompt_line > indicator_line && prompt_line < status_line,
        "composer prompt must sit between the indicator and the status row \
         (indicator={indicator_line}, prompt={prompt_line}, status={status_line}):\n{rendered}",
    );
}

#[test]
fn working_inline_segment_is_kept_when_indicator_is_suppressed() {
    // Small-screen fallback: when show_indicator=false (e.g. terminal
    // below INDICATOR_SCREEN_FLOOR), the status bar must keep the inline
    // `⠴ Working Ns` spinner so the user still sees a busy signal.
    let mut pane = BottomPane::new();
    pane.update_status(working_status(false));

    let mut terminal = Terminal::new(TestBackend::new(80, 6)).expect("terminal");
    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);
    let lines: Vec<&str> = rendered.lines().collect();

    assert!(
        !rendered.contains("Ctrl+C to interrupt"),
        "indicator row must stay suppressed in this scenario:\n{rendered}"
    );
    let status_line = lines
        .iter()
        .rposition(|line| line.contains("·  ⠴ Working"))
        .expect("status bar inline spinner should fall back when the indicator is hidden");
    assert_eq!(
        status_line,
        lines.len() - 1,
        "status bar should still occupy the bottom row:\n{rendered}"
    );
}

#[test]
fn indicator_row_is_suppressed_when_show_flag_is_off() {
    // Working state, but the main loop refused to allocate the row
    // (e.g. small screen). The row must not paint and must not steal a
    // layout slot — otherwise the composer would shift down.
    let mut ready_pane = BottomPane::new();
    ready_pane.update_status(ready_status());
    let mut ready_terminal = Terminal::new(TestBackend::new(80, 6)).expect("terminal");
    render(&ready_pane, &mut ready_terminal);
    let ready_prompt_line = rendered_text(&ready_terminal)
        .lines()
        .position(|line| line.contains('›'))
        .expect("baseline composer prompt should render");

    let mut pane = BottomPane::new();
    pane.update_status(working_status(false));
    let mut terminal = Terminal::new(TestBackend::new(80, 6)).expect("terminal");
    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);
    assert!(
        !rendered.contains("Ctrl+C to interrupt"),
        "indicator row should be suppressed when show_indicator=false:\n{rendered}"
    );

    // Composer prompt must land in the same row as the Ready baseline —
    // a suppressed indicator slot cannot shift the composer downward.
    let prompt_line = rendered
        .lines()
        .position(|line| line.contains('›'))
        .expect("composer prompt should render");
    assert_eq!(
        prompt_line, ready_prompt_line,
        "composer position must match the Ready case when indicator is hidden:\n{rendered}"
    );
}

#[test]
fn desired_height_grows_by_one_when_indicator_row_is_active() {
    let mut pane = BottomPane::new();
    pane.update_status(ready_status());
    let baseline = pane.desired_height(80);

    pane.update_status(working_status(true));
    let with_indicator = pane.desired_height(80);

    assert_eq!(
        with_indicator,
        baseline + 1,
        "indicator row must contribute exactly one row to desired_height"
    );

    // Hidden indicator must not contribute anything.
    pane.update_status(working_status(false));
    assert_eq!(
        pane.desired_height(80),
        baseline,
        "suppressed indicator must not occupy a layout slot"
    );
}

// --- History search (Ctrl+R) integration tests ---

#[test]
fn ctrl_r_opens_history_search_when_history_nonempty() {
    let mut pane = BottomPane::new();

    // Submit a prompt to populate history.
    type_text(&mut pane, "first prompt");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(pane.composer().history(), &["first prompt".to_string()]);

    // Ctrl+R should open the search overlay.
    let event = press(&mut pane, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(event, ComposerEvent::Nothing);
    assert!(pane.has_overlay(), "history search overlay should be open");
}

#[test]
fn ctrl_r_is_noop_on_empty_history() {
    let mut pane = BottomPane::new();

    let event = press(&mut pane, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(event, ComposerEvent::Nothing);
    assert!(!pane.has_overlay(), "no overlay on empty history");
}

#[test]
fn history_search_enter_selects_match_and_fills_composer() {
    let mut pane = BottomPane::new();

    type_text(&mut pane, "hello");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    type_text(&mut pane, "world");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);

    // Open history search — initial query is empty (composer is empty).
    let _ = press(&mut pane, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(pane.has_overlay());

    // Type to filter.
    press(&mut pane, KeyCode::Char('h'), KeyModifiers::NONE);

    // Select the match and confirm.
    let event = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(event, ComposerEvent::Nothing);
    assert!(!pane.has_overlay(), "overlay should close on Enter");
    assert_eq!(pane.composer().text(), "hello");
}

#[test]
fn history_search_esc_restores_pre_search_buffer() {
    let mut pane = BottomPane::new();

    type_text(&mut pane, "saved");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);

    // Type something before opening search.
    type_text(&mut pane, "draft");

    // Open search.
    let _ = press(&mut pane, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(pane.has_overlay());

    // Esc should restore the pre-search buffer.
    let event = press(&mut pane, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(event, ComposerEvent::Nothing);
    assert!(!pane.has_overlay());
    assert_eq!(pane.composer().text(), "draft");
}

#[test]
fn history_search_up_down_navigate_matches() {
    let mut pane = BottomPane::new();

    type_text(&mut pane, "fix bug");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    type_text(&mut pane, "fix tests");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    type_text(&mut pane, "run tests");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);

    // Open search and filter.
    let _ = press(&mut pane, KeyCode::Char('r'), KeyModifiers::CONTROL);
    type_text(&mut pane, "fix");

    // Newest match ("fix tests") should be selected first.
    press(&mut pane, KeyCode::Down, KeyModifiers::NONE);

    // Select the older match.
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(pane.composer().text(), "fix bug");
}

#[test]
fn history_search_ctrl_r_cycles_to_older_match() {
    let mut pane = BottomPane::new();

    type_text(&mut pane, "alpha");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    type_text(&mut pane, "alpha beta");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);

    // Open search, initial query is empty → all history matches.
    let _ = press(&mut pane, KeyCode::Char('r'), KeyModifiers::CONTROL);

    // Ctrl+R again should cycle to the next (older) match.
    let _ = press(&mut pane, KeyCode::Char('r'), KeyModifiers::CONTROL);
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(pane.composer().text(), "alpha");
}

#[test]
fn history_search_renders_matching_entries() {
    let mut pane = BottomPane::new();
    let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("terminal");

    type_text(&mut pane, "first");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);
    type_text(&mut pane, "second");
    let _ = press(&mut pane, KeyCode::Enter, KeyModifiers::NONE);

    // Open search.
    let _ = press(&mut pane, KeyCode::Char('r'), KeyModifiers::CONTROL);
    render(&pane, &mut terminal);
    let rendered = rendered_text(&terminal);

    assert!(rendered.contains("bck-i-search"), "search prompt should render");
    assert!(rendered.contains("second"), "newest match should render: {rendered}");
    assert!(rendered.contains("first"), "oldest match should render: {rendered}");
}
