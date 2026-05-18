use nav_core::{
    AgentEvent, FileChangeKind, FileChangeSummary, FileDiffSummary, PatchApplyStatus, TurnUsage,
};
use nav_tui::ChatWidget;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::json;

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

#[test]
fn wrapped_skill_prompt_renders_as_skill_then_user_request() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::UserMessage {
        text: "<skill name=\"zoom-out\" dir=\"/Users/season/.agents/skills/zoom-out\">\nSkill body\n</skill>\n\nInspect the TUI modules.".to_string(),
        display_text: None,
    });

    let rendered = render_widget(&widget, 90, 10);

    assert!(rendered.contains("◆ skill  zoom-out"), "{rendered}");
    assert!(
        rendered.contains("› user  Inspect the TUI modules."),
        "{rendered}"
    );
    assert!(!rendered.contains("<skill"), "{rendered}");
    assert!(!rendered.contains("Skill body"), "{rendered}");
}

#[test]
fn wrapped_skill_prompt_uses_outer_closing_tag() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::UserMessage {
        text: "<skill name=\"zoom-out\" dir=\"/Users/season/.agents/skills/zoom-out\">\nDo not render this literal </skill> mention.\n</skill>\n\nInspect the TUI modules.".to_string(),
        display_text: None,
    });

    let rendered = render_widget(&widget, 90, 10);

    assert!(rendered.contains("◆ skill  zoom-out"), "{rendered}");
    assert!(
        rendered.contains("› user  Inspect the TUI modules."),
        "{rendered}"
    );
    assert!(!rendered.contains("Do not render"), "{rendered}");
}

#[test]
fn literal_skill_xml_without_nav_dir_attribute_stays_user_text() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::UserMessage {
        text: "<skill name=\"literal\">\nbody\n</skill>\n\nExplain this tag.".to_string(),
        display_text: None,
    });

    let rendered = render_widget(&widget, 90, 10);

    assert!(!rendered.contains("◆ skill"), "{rendered}");
    assert!(
        rendered.contains("› user  <skill name=\"literal\">"),
        "{rendered}"
    );
}

#[test]
fn tool_rows_label_skill_reads_and_truncate_known_outputs() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({
            "path": "/Users/season/.agents/skills/zoom-out/SKILL.md"
        }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_1".to_string(),
        output: (0..20)
            .map(|i| format!("line {i:02}"))
            .collect::<Vec<_>>()
            .join("\n"),
        is_error: false,
    });

    let rendered = render_widget(&widget, 90, 20);

    assert!(
        rendered.contains("• tool  read_file SKILL.md (zoom-out skill)"),
        "{rendered}"
    );
    assert!(rendered.contains("└ output  20 lines"), "{rendered}");
    assert!(rendered.contains("line 03"), "{rendered}");
    assert!(rendered.contains("… 16 more lines hidden"), "{rendered}");
    assert!(!rendered.contains("line 19"), "{rendered}");
}

#[test]
fn apply_patch_tool_row_summarizes_patch_paths() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "apply_patch".to_string(),
        arguments: json!({
            "patch": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** Add File: src/new.rs\n+hello\n*** End Patch\n"
        }),
    });

    let rendered = render_widget(&widget, 100, 8);

    assert!(
        rendered.contains("• tool  apply_patch M src/lib.rs, A src/new.rs"),
        "{rendered}"
    );
    assert!(!rendered.contains("*** Begin Patch"), "{rendered}");
}

#[test]
fn apply_patch_tool_row_summarizes_moves() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "apply_patch".to_string(),
        arguments: json!({
            "patch": "*** Begin Patch\n*** Update File: old.rs\n*** Move to: new.rs\n@@\n-old\n+new\n*** End Patch\n"
        }),
    });

    let rendered = render_widget(&widget, 100, 8);

    assert!(
        rendered.contains("• tool  apply_patch M old.rs -> new.rs"),
        "{rendered}"
    );
}

#[test]
fn file_change_event_renders_reviewable_diff_preview() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::FileChange {
        call_id: "call_1".to_string(),
        status: PatchApplyStatus::Completed,
        summary: "updated 1 file: M note.txt:1 (+1 -1)".to_string(),
        error: None,
        changes: vec![FileChangeSummary {
            path: "note.txt".to_string(),
            kind: FileChangeKind::Update { move_path: None },
            additions: 1,
            deletions: 1,
            line_start: Some(1),
            diff: "--- a/note.txt\n+++ b/note.txt\n@@\n-old\n+new\n".to_string(),
        }],
    });

    let rendered = render_widget(&widget, 100, 12);

    assert!(rendered.contains("◆ changed  updated 1 file"), "{rendered}");
    assert!(rendered.contains("M note.txt:1 (+1 -1)"), "{rendered}");
    assert!(rendered.contains("-old"), "{rendered}");
    assert!(rendered.contains("+new"), "{rendered}");
}

#[test]
fn turn_diff_event_renders_modified_file_summary() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::TurnDiff {
        truncated: false,
        files: vec![FileDiffSummary {
            path: "note.txt".to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 1,
        }],
        unified_diff: "--- a/note.txt\n+++ b/note.txt\n@@\n-old\n+new\n".to_string(),
    });

    let rendered = render_widget(&widget, 100, 12);

    assert!(rendered.contains("◆ diff  1 file changed"), "{rendered}");
    assert!(rendered.contains("modified note.txt (+1 -1)"), "{rendered}");
    assert!(rendered.contains("-old"), "{rendered}");
    assert!(rendered.contains("+new"), "{rendered}");
}

#[test]
fn labeled_rows_wrap_without_clipping_first_line() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::AssistantMessageDone {
        text: format!("{}LOSTMARK{}", "a".repeat(27), "b".repeat(20)),
    });

    let rendered = render_widget(&widget, 40, 8);

    assert!(rendered.contains("LOSTMARK"), "{rendered}");
}

#[test]
fn renders_full_turn_transcript() {
    let mut widget = ChatWidget::new();

    widget.push_user("list files in the repo");
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": ["ls"] }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_1".to_string(),
        output: "Cargo.toml\nsrc".to_string(),
        is_error: false,
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "Two entries: Cargo.toml and src.".to_string(),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            frame.render_widget(&widget, area);
        })
        .expect("draw");

    let rendered = buffer_to_text(terminal.backend().buffer());
    insta::assert_snapshot!("full_turn_transcript", rendered);
}

#[test]
fn overflowing_transcript_follows_newest_lines() {
    let widget = widget_with_numbered_output(20);

    let rendered = render_widget(&widget, 40, 6);

    assert!(!rendered.contains("line 00"), "{rendered}");
    assert!(rendered.contains("line 19"), "{rendered}");
}

#[test]
fn scroll_up_reveals_older_lines() {
    let mut widget = widget_with_numbered_output(20);

    widget.scroll_up(100, 40, 6);
    let rendered = render_widget(&widget, 40, 6);

    assert!(rendered.contains("line 00"), "{rendered}");
    assert!(!rendered.contains("line 19"), "{rendered}");
}

#[test]
fn scroll_down_returns_to_newest_lines() {
    let mut widget = widget_with_numbered_output(20);

    widget.scroll_up(100, 40, 6);
    widget.scroll_down(100, 40, 6);
    let rendered = render_widget(&widget, 40, 6);

    assert!(!rendered.contains("line 00"), "{rendered}");
    assert!(rendered.contains("line 19"), "{rendered}");
}

#[test]
fn scrolled_viewport_stays_stable_when_new_output_arrives() {
    let mut widget = widget_with_numbered_output(20);

    widget.scroll_up(5, 40, 6);
    let before = render_widget(&widget, 40, 6);
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_2".to_string(),
        output: "new line".to_string(),
        is_error: false,
    });
    let after = render_widget(&widget, 40, 6);

    assert!(before.contains("line 10"), "{before}");
    assert!(after.contains("line 10"), "{after}");
    assert!(!after.contains("new line"), "{after}");
}

#[test]
fn scrolling_before_overflow_keeps_following_new_output() {
    let mut widget = widget_with_numbered_output(2);

    widget.scroll_up(5, 40, 6);
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_2".to_string(),
        output: (2..20)
            .map(|i| format!("line {i:02}"))
            .collect::<Vec<_>>()
            .join("\n"),
        is_error: false,
    });
    let rendered = render_widget(&widget, 40, 6);

    assert!(!rendered.contains("line 00"), "{rendered}");
    assert!(rendered.contains("line 19"), "{rendered}");
}

#[test]
fn scroll_to_top_handles_transcripts_taller_than_u16() {
    let mut widget = widget_with_numbered_output(u16::MAX as usize + 5);

    widget.scroll_to_top(40, 6);
    let rendered = render_widget(&widget, 40, 6);

    assert!(rendered.contains("line 00"), "{rendered}");
    assert!(!rendered.contains("line 65539"), "{rendered}");
}
