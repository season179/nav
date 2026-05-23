use nav_core::{
    AgentEvent, FileChangeKind, FileChangeSummary, FileDiffSummary, GitCheckpointAction,
    GitCheckpointStatus, PatchApplyStatus, PendingInputMode, ReviewDecision, SessionSummary,
    TurnUsage, UserAttachment,
};
use nav_tui::ChatWidget;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::text::Line;
use serde_json::json;

fn lines_to_text(lines: &[Line<'_>]) -> String {
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            out.push_str(&span.content);
        }
        out.push('\n');
    }
    out
}

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

/// The runtime widget no longer renders finalized cells inline (they go
/// straight to native scrollback via `insert_history`). For snapshots we
/// drain the same lines the runtime would insert, paint them top-aligned
/// into a buffer, then overlay any in-flight streaming text below —
/// preserving the "what the user sees this turn" shape the old tests
/// asserted on, even though at runtime these rows live in two different
/// places.
fn render_widget(widget: &mut ChatWidget, width: u16, height: u16) -> String {
    use ratatui::widgets::{Paragraph, Widget};
    let mut lines = widget.drain_pending(width);
    // `inline_lines` covers both the streaming assistant cell and any
    // `Exploring`/`Running` tool placeholders — the latter no longer flow
    // through `drain_pending` (they live inline until the matching
    // `ToolCallOutput` arrives), so we have to overlay them explicitly.
    lines.extend(widget.inline_lines(width));

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            Paragraph::new(lines).render(area, frame.buffer_mut());
        })
        .expect("draw");
    buffer_to_text(terminal.backend().buffer())
}

#[test]
fn wrapped_skill_prompt_renders_as_skill_then_user_request() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::UserMessage {
        text: "<skill name=\"zoom-out\" dir=\"/Users/season/.agents/skills/zoom-out\">\nSkill body\n</skill>\n\nInspect the TUI modules.".to_string(),
        display_text: None,
        attachments: Vec::new(),
    });

    let rendered = render_widget(&mut widget, 90, 10);

    assert!(rendered.contains("◆ skill  zoom-out"), "{rendered}");
    assert!(
        rendered.contains("› Inspect the TUI modules."),
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
        attachments: Vec::new(),
    });

    let rendered = render_widget(&mut widget, 90, 10);

    assert!(rendered.contains("◆ skill  zoom-out"), "{rendered}");
    assert!(
        rendered.contains("› Inspect the TUI modules."),
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
        attachments: Vec::new(),
    });

    let rendered = render_widget(&mut widget, 90, 10);

    assert!(!rendered.contains("◆ skill"), "{rendered}");
    assert!(
        rendered.contains("› <skill name=\"literal\">"),
        "{rendered}"
    );
}

#[test]
fn user_message_attachments_render_in_submitted_box() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::UserMessage {
        text: "See attached".to_string(),
        display_text: None,
        attachments: vec![
            UserAttachment::Image {
                path: ".nav/clipboard/shot.png".into(),
            },
            UserAttachment::File {
                path: "src/main.rs".into(),
            },
        ],
    });

    let rendered = render_widget(&mut widget, 80, 8);
    assert!(rendered.contains("› See attached"), "{rendered}");
    assert!(
        rendered.contains("  [image] .nav/clipboard/shot.png"),
        "{rendered}"
    );
    assert!(rendered.contains("  [file] src/main.rs"), "{rendered}");
}

#[test]
fn approval_decision_event_renders_audit_row() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallApprovalDecision {
        approval_id: "approval-1".to_string(),
        decision: ReviewDecision::ApprovedForSession,
    });

    let rendered = render_widget(&mut widget, 80, 4);
    assert!(
        rendered.contains("✓ approved matching tool calls for this session"),
        "{rendered}"
    );
}

#[test]
fn pure_conversation_turn_complete_does_not_render_separator() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::UserMessage {
        text: "hello".to_string(),
        display_text: None,
        attachments: Vec::new(),
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "Hi there.".to_string(),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage {
            tokens_input: 10,
            tokens_output: 2,
            ..TurnUsage::default()
        },
    });

    let rendered = render_widget(&mut widget, 80, 10);
    assert!(rendered.contains("• Hi there."), "{rendered}");
    assert!(!rendered.contains("─ 10 in, 2 out"), "{rendered}");
    assert!(!rendered.contains("turn complete"), "{rendered}");
}

#[test]
fn tool_rows_collapse_skill_reads_into_summary_and_drop_output_preview() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({
            "path": "/Users/season/.agents/skills/zoom-out/SKILL.md"
        }),
    });
    // While the call is in flight the grouped row shows the friendly
    // `SKILL.md (<skill> skill)` form of the path as the current target.
    let started = render_widget(&mut widget, 90, 20);
    assert!(
        started.contains("Exploring (1 call)"),
        "in-flight group must show call count; got:\n{started}"
    );
    assert!(
        started.contains("SKILL.md (zoom-out skill)"),
        "current target must surface the skill-aware display path; got:\n{started}"
    );

    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_1".to_string(),
        output: (0..20)
            .map(|i| format!("line {i:02}"))
            .collect::<Vec<_>>()
            .join("\n"),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let rendered = lines_to_text(&widget.drain_pending(90));

    assert!(
        rendered.contains("Exploring (1 call)"),
        "scrollback must show the collapsed exploring group; got:\n{rendered}"
    );
    // The 20-line tool output is not surfaced — the summary is a count, not a preview.
    assert!(!rendered.contains("  └ 20 lines"), "{rendered}");
    assert!(!rendered.contains("line 03"), "{rendered}");
    assert!(!rendered.contains("line 19"), "{rendered}");
}

#[test]
fn mixed_explorations_collapse_into_one_summary_row() {
    let mut widget = ChatWidget::new();

    // Two reads and a search across the same turn. All of them — regardless
    // of action — fold into a single past-tense summary row when the turn
    // completes.
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "docs/a.md" }),
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_2".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "docs/b.md" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_1".to_string(),
        output: "content a".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_2".to_string(),
        output: "content b".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_3".to_string(),
        name: "code_search".to_string(),
        arguments: json!({ "pattern": "inserthistory", "path": "src" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_3".to_string(),
        output: "match".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let rendered = lines_to_text(&widget.drain_pending(100));

    assert!(
        rendered.contains("Exploring (3 calls)"),
        "all read-only calls must fold into one group; got:\n{rendered}"
    );
    assert_eq!(
        rendered.matches("Exploring (3 calls)").count(),
        1,
        "expected exactly one group row; got:\n{rendered}"
    );
    // Collapsed: only the most recent target is visible.
    assert!(!rendered.contains("docs/a.md"), "{rendered}");
    assert!(rendered.contains("inserthistory"), "{rendered}");
    assert!(!rendered.contains("  └ 1 line"), "{rendered}");
}

#[test]
fn explorations_dedupe_within_a_round_then_flush_per_assistant_segment() {
    // A round's reads dedupe and collapse into one summary. When the model
    // starts a new streaming text segment, the buffered summary flushes
    // first so each segment's tool work appears immediately above the
    // text that follows it. Re-reads of the same path in a later segment
    // count separately because they belong to a different chunk of work.
    let mut widget = ChatWidget::new();

    // Segment 1: read a.md and b.md
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "a.md" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r1".to_string(),
        output: "aaa".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1b".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "b.md" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r1b".to_string(),
        output: "bbb".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "let me read more".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "let me read more".to_string(),
    });

    // Segment 2: read b.md again (different segment — counts separately)
    // plus c.md.
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r2".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "b.md" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r2".to_string(),
        output: "bbb".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r3".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "c.md" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r3".to_string(),
        output: "ccc".to_string(),
        is_error: false,
        truncation: None,
    });

    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let rendered = lines_to_text(&widget.drain_pending(100));

    // Two group rows, one per segment (two reads each).
    assert_eq!(
        rendered.matches("Exploring (2 calls)").count(),
        2,
        "expected one group row per segment; got:\n{rendered}"
    );
    // Segment 1's row must appear ABOVE the assistant text it preceded.
    let seg1_idx = rendered
        .find("Exploring (2 calls)")
        .expect("segment 1 group present");
    let text_idx = rendered
        .find("let me read more")
        .expect("assistant text present");
    assert!(
        seg1_idx < text_idx,
        "segment 1 summary must land before the streaming text; got:\n{rendered}"
    );
}

#[test]
fn consecutive_reads_group_breaks_on_write_tool() {
    let mut widget = ChatWidget::new();

    for (id, path) in [("r1", "a.rs"), ("r2", "b.rs")] {
        widget.ingest(AgentEvent::ToolCallStarted {
            call_id: id.to_string(),
            name: "read_file".to_string(),
            arguments: json!({ "path": path }),
        });
        widget.ingest(AgentEvent::ToolCallOutput {
            call_id: id.to_string(),
            output: "ok".to_string(),
            is_error: false,
            truncation: None,
        });
    }

    let inline_before_write = lines_to_text(&widget.inline_lines(100));
    assert!(
        inline_before_write.contains("Exploring (2 calls)"),
        "buffered reads must show as one inline group; got:\n{inline_before_write}"
    );

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "w1".to_string(),
        name: "apply_patch".to_string(),
        arguments: json!({
            "patch": "*** Begin Patch\n*** Update File: b.rs\n@@\n-ok\n+done\n*** End Patch\n"
        }),
    });

    let scrollback = lines_to_text(&widget.drain_pending(100));
    assert!(
        scrollback.contains("Exploring (2 calls)"),
        "starting a write must flush the read group to scrollback; got:\n{scrollback}"
    );
    let inline = lines_to_text(&widget.inline_lines(100));
    assert!(
        inline.contains("apply_patch"),
        "write tool must render outside the group; got:\n{inline}"
    );
}

#[test]
fn exploration_buffer_flushes_when_apply_patch_starts() {
    // Non-read-only tool start breaks the group so scrollback reads
    // explore → modify.
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "foo.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r1".to_string(),
        output: "fn main()".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "p1".to_string(),
        name: "apply_patch".to_string(),
        arguments: json!({
            "patch": "*** Begin Patch\n*** Update File: foo.rs\n@@\n-a\n+b\n*** End Patch\n"
        }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "p1".to_string(),
        output: "updated 1 file".to_string(),
        is_error: false,
        truncation: None,
    });

    let rendered = lines_to_text(&widget.drain_pending(100));

    assert!(
        rendered.contains("Exploring (1 call)"),
        "exploring group must appear before the patch row; got:\n{rendered}"
    );
    assert!(
        rendered.contains("• Ran  apply_patch M foo.rs"),
        "{rendered}"
    );
    let summary_idx = rendered.find("Exploring (1 call)").expect("group present");
    let patch_idx = rendered.find("• Ran  apply_patch").expect("patch present");
    assert!(
        summary_idx < patch_idx,
        "summary must flush ahead of the patch row; got:\n{rendered}"
    );
}

#[test]
fn explorations_flush_before_next_streaming_assistant_cell() {
    // Regression: with drain_pending no longer flushing, a buffered Read
    // could survive across a streaming assistant message and land BELOW
    // it in scrollback (or worse, at TurnComplete). When a new streaming
    // cell starts, the previous round's exploration summary must already
    // be in scrollback so causality is preserved.
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "config.toml" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r1".to_string(),
        output: "ok".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "Based on config.toml, ".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "Based on config.toml, …".to_string(),
    });

    let scrollback = lines_to_text(&widget.drain_pending(100));

    let summary_idx = scrollback
        .find("Exploring (1 call)")
        .expect("group present in scrollback");
    let text_idx = scrollback
        .find("Based on config.toml")
        .expect("assistant text present");
    assert!(
        summary_idx < text_idx,
        "exploration summary must precede the next streaming text; got:\n{scrollback}"
    );
}

#[test]
fn same_file_read_twice_across_frame_drain_stays_in_one_group() {
    // Regression: two `read_file` calls on the same path used to produce
    // two separate rows because `drain_pending` flushed between them.
    // Consecutive reads should stay in one exploring group with call count.
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "c1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "crates/nav-tui/src/cells/wrapping.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "c1".to_string(),
        output: "first content".to_string(),
        is_error: false,
        truncation: None,
    });

    // Simulate the runtime draining frames between the two reads — this is
    // what currently produces the duplicate Explored row.
    let mut scrollback: Vec<Line<'static>> = widget.drain_pending(100);

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "c2".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "crates/nav-tui/src/cells/wrapping.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "c2".to_string(),
        output: "second content".to_string(),
        is_error: false,
        truncation: None,
    });

    scrollback.extend(widget.drain_pending(100));

    // End-of-turn flushes whatever's still buffered into scrollback.
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });
    scrollback.extend(widget.drain_pending(100));

    let rendered = lines_to_text(&scrollback);

    assert!(
        rendered.contains("Exploring (2 calls)"),
        "expected one group counting both reads; got:\n{rendered}"
    );
    assert_eq!(
        rendered.matches("Exploring (2 calls)").count(),
        1,
        "expected exactly one group row; got:\n{rendered}"
    );
}

#[test]
fn inline_running_summary_shows_present_tense_with_current_target() {
    // Mirrors image 3 in the design note: while exploration tools are
    // still in flight (or buffered, post-output, pre-flush), the inline
    // viewport shows a single comma-joined present-tense summary and
    // the most recently started target on the row below.
    let mut widget = ChatWidget::new();

    // One search just completed — buffered into exploring_buffer.
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "s1".to_string(),
        name: "code_search".to_string(),
        arguments: json!({ "pattern": "TODO", "path": "src" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "s1".to_string(),
        output: "match".to_string(),
        is_error: false,
        truncation: None,
    });
    // Three different files being read; the second one is the most
    // recently started in-flight tool and should be the displayed target.
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "a.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r1".to_string(),
        output: "x".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r2".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "b.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r3".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "crates/nav-tui/tests/snapshot.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "l1".to_string(),
        name: "list_files".to_string(),
        arguments: json!({ "path": "src" }),
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "b1".to_string(),
        name: "bash".to_string(),
        arguments: json!({ "command": "cargo build" }),
    });

    let inline = lines_to_text(&widget.inline_lines(120));

    assert!(
        inline.contains("Exploring (6 calls)"),
        "inline group must count buffered and in-flight read-only calls; got:\n{inline}"
    );
    assert!(
        inline.contains("cargo build"),
        "most recently started in-flight target should appear under the summary; got:\n{inline}"
    );
    assert!(
        !inline.contains("• Exploring\n  Read"),
        "individual read placeholders must be collapsed into the group; got:\n{inline}"
    );
    assert!(
        !inline.contains("• Running  cargo build"),
        "individual `Running` placeholder for bash must be collapsed into the summary; got:\n{inline}"
    );
}

#[test]
fn inline_summary_switches_to_past_tense_when_no_tool_in_flight() {
    // Between two batches of tool calls (or between the last read and the
    // next assistant message), exploring_buffer is non-empty but no
    // summary tool is currently in flight. The inline cell must NOT claim
    // "Reading…" because nothing is being read right now.
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "foo.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r1".to_string(),
        output: "ok".to_string(),
        is_error: false,
        truncation: None,
    });

    let inline = lines_to_text(&widget.inline_lines(80));

    assert!(
        inline.contains("Exploring (1 call)"),
        "buffered read must show as a grouped row; got:\n{inline}"
    );
    assert!(
        inline.contains("└ foo.rs"),
        "collapsed group shows the most recent target; got:\n{inline}"
    );
}

#[test]
fn successful_bash_folds_into_summary_as_shell_command() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "b1".to_string(),
        name: "bash".to_string(),
        arguments: json!({ "command": "cargo test" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "b1".to_string(),
        output: "test result: ok".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let rendered = lines_to_text(&widget.drain_pending(100));

    assert!(
        rendered.contains("Exploring (1 call)"),
        "successful bash must join the exploring group; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("• Ran  cargo test"),
        "successful bash must not produce its own Ran row; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("test result: ok"),
        "successful bash output preview must be hidden; got:\n{rendered}"
    );
}

#[test]
fn failed_bash_still_shows_output_preview() {
    // Failure paths bypass the summary so the user keeps seeing the actual
    // command and the error output — same as failed exploration tools.
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "b1".to_string(),
        name: "bash".to_string(),
        arguments: json!({ "command": "exit 1" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "b1".to_string(),
        output: "boom".to_string(),
        is_error: true,
        truncation: None,
    });

    let rendered = lines_to_text(&widget.drain_pending(100));

    assert!(
        rendered.contains("■ Failed  exit 1"),
        "failed bash must keep its dedicated row; got:\n{rendered}"
    );
    assert!(rendered.contains("boom"), "{rendered}");
}

#[test]
fn failed_read_after_successful_reads_flushes_group_then_shows_error() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "ok".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "good.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "ok".to_string(),
        output: "fine".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "bad".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "nope.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "bad".to_string(),
        output: "permission denied".to_string(),
        is_error: true,
        truncation: None,
    });

    let rendered = lines_to_text(&widget.drain_pending(100));

    assert!(
        rendered.contains("Exploring (1 call)"),
        "successful read must flush as a group before the failure row; got:\n{rendered}"
    );
    assert!(
        rendered.contains("■ Failed  Read nope.rs"),
        "failed read must keep its own output row; got:\n{rendered}"
    );
    let group_idx = rendered.find("Exploring (1 call)").expect("group");
    let fail_idx = rendered.find("■ Failed").expect("failed row");
    assert!(
        group_idx < fail_idx,
        "group must precede the failure row; got:\n{rendered}"
    );
}

#[test]
fn failed_exploration_does_not_collapse() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "f1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "nope.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "f1".to_string(),
        output: "permission denied".to_string(),
        is_error: true,
        truncation: None,
    });

    let rendered = render_widget(&mut widget, 100, 20);

    // Failed exploration goes through ToolOutputCell, not the collapsed path
    assert!(
        rendered.contains("■ Failed  Read nope.rs"),
        "{rendered}"
    );
    assert!(rendered.contains("  └ 1 line"), "{rendered}");
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

    let rendered = render_widget(&mut widget, 100, 8);

    assert!(
        rendered.contains("• Running  apply_patch M src/lib.rs, A src/new.rs"),
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

    let rendered = render_widget(&mut widget, 100, 8);

    assert!(
        rendered.contains("• Running  apply_patch M old.rs -> new.rs"),
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

    let rendered = render_widget(&mut widget, 100, 12);

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

    let rendered = render_widget(&mut widget, 100, 12);

    assert!(rendered.contains("◆ diff  1 file changed"), "{rendered}");
    assert!(rendered.contains("modified note.txt (+1 -1)"), "{rendered}");
    assert!(rendered.contains("-old"), "{rendered}");
    assert!(rendered.contains("+new"), "{rendered}");
}

#[test]
fn git_checkpoint_event_renders_restore_handle() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::GitCheckpoint {
        action: GitCheckpointAction::Checkpoint,
        status: GitCheckpointStatus::Created,
        stash_ref: Some("stash@{0}".to_string()),
        stash_oid: Some("1234567890abcdef".to_string()),
        message: "nav checkpoint 01ABCDEF: before turn".to_string(),
    });

    let rendered = render_widget(&mut widget, 100, 8);

    assert!(rendered.contains("◆ checkpoint  created"), "{rendered}");
    assert!(rendered.contains("stash@{0} (1234567890ab)"), "{rendered}");
    assert!(rendered.contains("nav checkpoint 01ABCDEF"), "{rendered}");
}

#[test]
fn pending_queue_and_abort_events_render_as_control_rows() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::PendingInputQueued {
        id: "pending-1".to_string(),
        mode: PendingInputMode::FollowUp,
        text: "run tests next".to_string(),
        display_text: None,
        attachments: Vec::new(),
        skill_name: Some("tdd".to_string()),
    });
    widget.ingest(AgentEvent::PendingInputEdited {
        id: "pending-1".to_string(),
        text: "run focused tests next".to_string(),
        display_text: None,
        attachments: Vec::new(),
        skill_name: Some("tdd".to_string()),
    });
    widget.ingest(AgentEvent::PendingInputDequeued {
        id: "pending-1".to_string(),
        mode: PendingInputMode::FollowUp,
    });
    widget.ingest(AgentEvent::TurnAborted {
        turn_id: "turn-1".to_string(),
        reason: "user interrupt".to_string(),
    });

    let rendered = render_widget(&mut widget, 100, 16);

    assert!(
        rendered.contains("◆ queued  pending-1 follow-up"),
        "{rendered}"
    );
    assert!(rendered.contains("run tests next"), "{rendered}");
    assert!(rendered.contains("tdd skill"), "{rendered}");
    assert!(rendered.contains("◆ edited  pending-1"), "{rendered}");
    assert!(rendered.contains("run focused tests next"), "{rendered}");
    assert!(
        rendered.contains("◆ dequeued  pending-1 follow-up"),
        "{rendered}"
    );
    assert!(
        rendered.contains("◆ aborted  turn-1 user interrupt"),
        "{rendered}"
    );
}

#[test]
fn subagent_lifecycle_events_render_as_rows() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::SubagentStarted {
        id: "call_worker".to_string(),
        label: Some("explorer".to_string()),
        task: "inspect session code".to_string(),
    });
    widget.ingest(AgentEvent::SubagentCompleted {
        id: "call_worker".to_string(),
        summary: "checked session/mod.rs".to_string(),
    });
    widget.ingest(AgentEvent::SubagentFailed {
        id: "call_other".to_string(),
        message: "model returned no summary".to_string(),
    });

    let rendered = render_widget(&mut widget, 100, 14);

    assert!(
        rendered.contains("* subagent  explorer (call_worker) started"),
        "{rendered}"
    );
    assert!(rendered.contains("inspect session code"), "{rendered}");
    assert!(
        rendered.contains("* subagent  explorer (call_worker) completed"),
        "{rendered}"
    );
    assert!(rendered.contains("checked session/mod.rs"), "{rendered}");
    assert!(
        rendered.contains("* subagent  call_other failed"),
        "{rendered}"
    );
    assert!(rendered.contains("model returned no summary"), "{rendered}");
}

#[test]
fn assistant_deltas_paint_incrementally_then_finalize() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "Hello, ".to_string(),
    });
    let mid = render_widget(&mut widget, 60, 6);
    assert!(mid.contains("• Hello,"), "{mid}");

    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "world!".to_string(),
    });
    let mid2 = render_widget(&mut widget, 60, 6);
    assert!(mid2.contains("Hello, world!"), "{mid2}");

    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "Hello, world!".to_string(),
    });
    let done = render_widget(&mut widget, 60, 6);
    assert!(done.contains("• Hello, world!"), "{done}");
    let count = done.matches("• Hello, world!").count();
    assert_eq!(count, 1, "expected a single assistant row, got:\n{done}");
}

#[test]
fn assistant_done_without_deltas_still_renders_full_text() {
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "resumed text".to_string(),
    });
    let rendered = render_widget(&mut widget, 60, 6);
    assert!(rendered.contains("• resumed text"), "{rendered}");
}

#[test]
fn tool_call_mid_stream_finalizes_open_assistant_cell() {
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "thinking about it".to_string(),
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "shell".to_string(),
        arguments: json!({ "command": ["ls"] }),
    });
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "second message".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "second message".to_string(),
    });

    let rendered = render_widget(&mut widget, 60, 12);
    assert!(rendered.contains("thinking about it"), "{rendered}");
    assert!(rendered.contains("second message"), "{rendered}");
    assert_eq!(
        rendered.matches("• thinking about it").count()
            + rendered.matches("• second message").count(),
        2,
        "expected two separate assistant rows, got:\n{rendered}"
    );
    // The tool call's `Running` placeholder lives inline (not in scrollback)
    // until its matching `ToolCallOutput` arrives, so it appears below the
    // finalized assistant messages in this rendered overlay.
    assert!(rendered.contains("• Running"), "{rendered}");
    let first_idx = rendered.find("thinking about it").unwrap();
    let second_idx = rendered.find("second message").unwrap();
    assert!(first_idx < second_idx, "{rendered}");
}

#[test]
fn pending_input_mid_stream_keeps_single_assistant_cell() {
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "Hello ".to_string(),
    });
    widget.ingest(AgentEvent::PendingInputQueued {
        id: "pending-1".to_string(),
        mode: PendingInputMode::FollowUp,
        text: "run tests next".to_string(),
        display_text: None,
        attachments: Vec::new(),
        skill_name: None,
    });
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "world!".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "Hello world!".to_string(),
    });

    let rendered = render_widget(&mut widget, 70, 12);
    // With the new scrollback architecture all cells write into native
    // scrollback in arrival order. The previous behaviour of "splice
    // streaming cell back to its anchor" is gone because we no longer
    // hold an in-memory transcript. The single-assistant-cell invariant
    // still holds (one `• Hello world!`); we just no longer reorder it
    // ahead of an interleaved queue row.
    assert_eq!(
        rendered.matches("• Hello world!").count(),
        1,
        "pending-input mid-stream must not split assistant text into two cells:\n{rendered}"
    );
    assert!(rendered.contains("◆ queued"), "{rendered}");
    let queue_idx = rendered.find("◆ queued").expect("queue row present");
    let assistant_idx = rendered
        .find("Hello world!")
        .expect("assistant message present");
    assert!(
        queue_idx < assistant_idx,
        "with scrollback architecture, queue row arrives before the streaming cell finalizes:\n{rendered}"
    );
}

#[test]
fn local_helpers_mid_stream_flush_streaming_first() {
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "first reply".to_string(),
    });
    widget.push_skill("zoom-out", "applied to this turn");
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "second reply".to_string(),
    });

    let rendered = render_widget(&mut widget, 70, 12);
    let first_idx = rendered.find("first reply").expect("first assistant text");
    let skill_idx = rendered.find("◆ skill").expect("skill row");
    let second_idx = rendered
        .find("second reply")
        .expect("second assistant text");
    assert!(
        first_idx < skill_idx && skill_idx < second_idx,
        "expected chronological order assistant→skill→assistant, got:\n{rendered}"
    );
}

#[test]
fn turn_aborted_mid_stream_preserves_partial_text() {
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "partial thought".to_string(),
    });
    widget.ingest(AgentEvent::TurnAborted {
        turn_id: "turn-1".to_string(),
        reason: "user interrupt".to_string(),
    });

    let rendered = render_widget(&mut widget, 70, 10);
    assert!(rendered.contains("partial thought"), "{rendered}");
    assert!(
        rendered.contains("◆ aborted  turn-1 user interrupt"),
        "{rendered}"
    );
}

#[test]
fn labeled_rows_wrap_without_clipping_first_line() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::AssistantMessageDone {
        text: format!("{}LOSTMARK{}", "a".repeat(27), "b".repeat(20)),
    });

    let rendered = render_widget(&mut widget, 40, 8);

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
        truncation: None,
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "Two entries: Cargo.toml and src.".to_string(),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let rendered = render_widget(&mut widget, 60, 20);
    insta::assert_snapshot!("full_turn_transcript", rendered);
}

#[test]
fn renders_session_management_cells() {
    let mut widget = ChatWidget::new();

    widget.push_session_notice("name", "Session name set to \"release work\"");
    widget.push_session_notice("export", "Wrote transcript to transcript.md");

    let rendered = render_widget(&mut widget, 96, 20);
    insta::assert_snapshot!("session_management_cells", rendered);
}

#[test]
fn turn_complete_folds_unresolved_read_only_inflight_into_exploring_group() {
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "orphan.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r2".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "other.rs" }),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let rendered = lines_to_text(&widget.drain_pending(80));
    assert!(
        rendered.contains("Exploring (2 calls)"),
        "turn end must fold unresolved read-only placeholders into one group; got:\n{rendered}"
    );
    assert!(
        rendered.contains("other.rs"),
        "collapsed group must show the most recently started target; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("orphan.rs"),
        "collapsed group must not list earlier targets; got:\n{rendered}"
    );
}

#[test]
fn turn_aborted_flushes_inflight_tool_placeholder_to_scrollback() {
    // Regression: a tool call that started but never produced output used
    // to leak its `Exploring` placeholder into `inflight_tool_calls`
    // forever, repainting on every later frame across every later turn.
    // `TurnAborted` must drain the placeholder into scrollback so the
    // inline viewport returns to a clean state.
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_orphan".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "/tmp/x.txt" }),
    });
    widget.ingest(AgentEvent::TurnAborted {
        turn_id: "turn-1".to_string(),
        reason: "user interrupt".to_string(),
    });

    let rendered = render_widget(&mut widget, 80, 10);
    assert!(
        rendered.contains("• Exploring"),
        "drained placeholder must land in scrollback; got:\n{rendered}"
    );
    assert!(
        rendered.contains("◆ aborted  turn-1 user interrupt"),
        "{rendered}"
    );
    // The inline overlay should now be empty — the placeholder lives in
    // `finalized`, not in `inflight_tool_calls`.
    assert_eq!(
        widget.inline_lines(80).len(),
        0,
        "inflight_tool_calls must be empty after TurnAborted"
    );
}

#[test]
fn error_event_flushes_inflight_tool_placeholder_to_scrollback() {
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_orphan".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "/tmp/x.txt" }),
    });
    widget.ingest(AgentEvent::Error {
        message: "transport dropped".to_string(),
    });

    let rendered = render_widget(&mut widget, 80, 10);
    assert!(
        rendered.contains("• Exploring"),
        "drained placeholder must land in scrollback; got:\n{rendered}"
    );
    assert!(rendered.contains("transport dropped"), "{rendered}");
    assert_eq!(widget.inline_lines(80).len(), 0);
}

#[test]
fn session_rewound_drops_inflight_placeholder_silently() {
    // Rewind explicitly undoes the events that spawned the placeholder, so
    // unlike `TurnAborted`/`Error` we drop it WITHOUT flushing to scrollback.
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_undone".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "/tmp/x.txt" }),
    });
    widget.ingest(AgentEvent::SessionRewound {
        target_seq: 5,
        removed_events: 3,
        preview: "user msg".to_string(),
    });

    let rendered = render_widget(&mut widget, 80, 10);
    assert!(
        !rendered.contains("• Exploring"),
        "rewind must drop placeholders without resurrecting them in scrollback; got:\n{rendered}"
    );
    assert!(rendered.contains("◆ rewind"), "{rendered}");
    assert_eq!(widget.inline_lines(80).len(), 0);
}

#[test]
fn inflight_tool_placeholders_preserve_arrival_order() {
    // Regression: with `BTreeMap<String, _>` the placeholders rendered in
    // lexicographic call-id order. OpenAI Responses call_ids are opaque
    // random strings, so lex order is effectively random. With a Vec, two
    // observables tell us the order is preserved:
    //   1. Inline: the most-recently-started target is the one shown
    //      under the running summary.
    //   2. Scrollback: when an abort drains the in-flight placeholders to
    //      finalized cells, they land in arrival order.
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "z_first".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "/first.txt" }),
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "a_second".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "/second.txt" }),
    });

    let inline = lines_to_text(&widget.inline_lines(80));
    assert!(
        inline.contains("Exploring (2 calls)"),
        "group counts both in-flight reads; got:\n{inline}"
    );
    assert!(
        inline.contains("/second.txt"),
        "most recently started target (a_second → /second.txt) drives the \
         current_target line; with BTreeMap ordering this would be \
         /first.txt instead. Got:\n{inline}"
    );
    assert!(
        !inline.contains("/first.txt"),
        "only the most recent target is surfaced inline; got:\n{inline}"
    );

    // Abort drains the placeholders to scrollback in arrival order.
    widget.ingest(AgentEvent::TurnAborted {
        turn_id: "t1".to_string(),
        reason: "stop".to_string(),
    });
    let scrollback = lines_to_text(&widget.drain_pending(80));
    let first_idx = scrollback.find("first.txt").expect("first placeholder");
    let second_idx = scrollback.find("second.txt").expect("second placeholder");
    assert!(
        first_idx < second_idx,
        "arrival order (z_first, a_second) must beat lex order in \
         scrollback; got:\n{scrollback}"
    );
}

#[test]
fn chat_widget_commit_tick_releases_stable_lines() {
    // The wiring contract: ChatWidget::on_commit_tick must propagate to the
    // streaming cell's chunking policy, otherwise stable lines stay hidden
    // forever and the user sees only the live tail. If a future refactor
    // drops the call from the app loop *or* breaks the delegation from
    // ChatWidget to AssistantMessageCell, this test catches it — without
    // ticks, "hello" stays gated; with one tick under smooth mode, it
    // becomes visible.
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "hello\nworld\n".to_string(),
    });

    let pre_tick = render_lines(&widget.inline_lines(80));
    assert!(
        !pre_tick.contains("hello"),
        "stable line leaked before commit-tick ran; got:\n{pre_tick}"
    );

    let advanced = widget.on_commit_tick();
    assert!(advanced, "commit tick must report progress when queue has units");

    let post_tick = render_lines(&widget.inline_lines(80));
    assert!(
        post_tick.contains("hello"),
        "commit tick failed to release the first stable line; got:\n{post_tick}"
    );
}

fn render_lines(lines: &[ratatui::text::Line<'static>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn inline_lines_capped_preserves_placeholders_over_long_stream() {
    // Regression: ratatui's `Paragraph` silently clips rows beyond its
    // chunk height. When `inline_lines` returned (streaming + placeholders)
    // and the chunk was capped at MAX_STREAMING_ROWS, the placeholders at
    // the tail of the Vec were invisibly clipped. The capped variant must
    // keep them visible by head-clipping the streaming cell instead.
    let mut widget = ChatWidget::new();
    // Each line ends with `\n` so the StreamController flushes it to the
    // stable half (otherwise it sits in the tail and the cell collapses to
    // one row).
    let long_stream = (0..40)
        .map(|i| format!("line {i:02}\n"))
        .collect::<String>();
    widget.ingest(AgentEvent::AssistantMessageDelta { text: long_stream });
    // Drive a commit tick so the chunking layer releases the stable lines.
    // 40 queued source lines is well over ENTER_QUEUE_DEPTH_LINES (8), so
    // the policy enters catch-up and batches the whole reveal in one tick.
    // Without this the stable region stays hidden behind the smoothing
    // gate and the cell would only show its (empty) tail.
    widget.on_commit_tick();
    // `inline_lines` (uncapped) materializes everything — by construction
    // it must exceed the cap we'll pass below, otherwise the test exercises
    // the wrong branch.
    let uncapped_streaming_only = widget.inline_lines(80).len();
    assert!(
        uncapped_streaming_only > 8,
        "test setup: streaming cell must overflow the cap (got {uncapped_streaming_only} lines)"
    );

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_visible".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "/visible.txt" }),
    });

    let capped = widget.inline_lines_capped(80, 8);
    assert!(
        capped.len() <= 8,
        "must not exceed cap; got {} lines",
        capped.len()
    );
    let rendered: String = capped
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("visible.txt"),
        "tool-call placeholder must survive the cap; got:\n{rendered}"
    );
    assert!(
        !rendered.contains("line 00"),
        "streaming head should be clipped, not the tail; got:\n{rendered}"
    );
}

#[test]
fn finalized_exploration_summary_uses_bullet_chrome_not_explored_label() {
    // The inline running summary already renders as a bare bullet
    // ("• reading 3 files…"). When it transitions to scrollback the user
    // should perceive it as the same row, not a re-labeled "Explored" row
    // with double-space chrome — that broke visual continuity and made
    // the same data look like two different cells.
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "foo.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r1".to_string(),
        output: "ok".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.flush_pending_for_shutdown();

    let scrollback = lines_to_text(&widget.drain_pending(80));

    assert!(
        !scrollback.contains("Explored"),
        "summary row must drop the 'Explored' label; got:\n{scrollback}"
    );
    assert!(
        scrollback.contains("Exploring (1 call)"),
        "finalized group must use the exploring header; got:\n{scrollback}"
    );
}

#[test]
fn flush_pending_for_shutdown_promotes_buffered_explorations() {
    // Regression: the AppEvent::Quit handler used to `break` out of the
    // main loop while exploring_buffer still held un-flushed calls,
    // so the running summary never made it into scrollback. ChatWidget
    // exposes `flush_pending_for_shutdown` so the app can promote those
    // groups into a finalized cell that the final drain_pending picks up.
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({ "path": "foo.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r1".to_string(),
        output: "ok".to_string(),
        is_error: false,
        truncation: None,
    });

    // Before flush: nothing finalized yet; the summary lives only inline.
    let before = lines_to_text(&widget.drain_pending(80));
    assert!(
        !before.contains("Exploring (1 call)"),
        "buffered exploration must not auto-flush via drain_pending; got:\n{before}"
    );

    widget.flush_pending_for_shutdown();

    let after = lines_to_text(&widget.drain_pending(80));
    assert!(
        after.contains("Exploring (1 call)"),
        "flush_pending_for_shutdown must surface the group; got:\n{after}"
    );
}

// ── Reasoning events ──────────────────────────────────────

#[test]
fn reasoning_done_renders_collapsed_reasoning_cell() {
    let mut widget = ChatWidget::new();

    widget.push_user("think about this");
    widget.ingest(AgentEvent::ReasoningDone {
        text: "I need to consider the trade-offs between option A and option B.\n\
               Option A is faster but less reliable.".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "I recommend option A.".to_string(),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let rendered = render_widget(&mut widget, 60, 20);
    insta::assert_snapshot!("reasoning_done_collapsed", rendered);
}

#[test]
fn reasoning_delta_then_done_builds_reasoning_cell() {
    let mut widget = ChatWidget::new();

    widget.push_user("solve this puzzle");

    // Streaming reasoning deltas arrive first.
    widget.ingest(AgentEvent::ReasoningDelta {
        text: "step 1: ".to_string(),
    });
    widget.ingest(AgentEvent::ReasoningDelta {
        text: "analyze the constraints\n".to_string(),
    });
    widget.ingest(AgentEvent::ReasoningDelta {
        text: "step 2: find solution".to_string(),
    });

    // Then the final coalesced reasoning text.
    widget.ingest(AgentEvent::ReasoningDone {
        text: "step 1: analyze the constraints\nstep 2: find solution".to_string(),
    });

    // Then the assistant reply.
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "The answer is 42.".to_string(),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let finalized = lines_to_text(&widget.drain_pending(60));
    assert!(
        finalized.contains("◆ reasoning"),
        "reasoning cell should carry the reasoning label; got:\n{finalized}"
    );
    assert!(
        finalized.contains("Reasoning (2 lines)"),
        "collapsed reasoning should show line count; got:\n{finalized}"
    );
}

#[test]
fn reasoning_delta_buffer_flushed_when_assistant_closes_without_done() {
    let mut widget = ChatWidget::new();

    widget.push_user("think");
    widget.ingest(AgentEvent::ReasoningDelta {
        text: "buffered only\nsecond line".to_string(),
    });
    // No ReasoningDone — provider may only stream deltas; assistant close
    // must still materialize a ReasoningCell from the buffer.
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "answer".to_string(),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let finalized = lines_to_text(&widget.drain_pending(60));
    assert!(
        finalized.contains("◆ reasoning"),
        "delta-only reasoning should flush on assistant close; got:\n{finalized}"
    );
    assert!(
        finalized.contains("Reasoning (2 lines)"),
        "buffered reasoning should collapse with line count; got:\n{finalized}"
    );
    assert!(
        !finalized.contains("• buffered only"),
        "reasoning must not leak under assistant bullet; got:\n{finalized}"
    );
}

#[test]
fn reasoning_cell_distinct_from_assistant_message() {
    let mut widget = ChatWidget::new();

    widget.push_user("test");
    widget.ingest(AgentEvent::ReasoningDone {
        text: "internal reasoning".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "public reply".to_string(),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    let finalized = lines_to_text(&widget.drain_pending(60));

    // Reasoning should appear with its own label, not as the assistant bullet.
    assert!(
        finalized.contains("◆ reasoning"),
        "reasoning must use its own label; got:\n{finalized}"
    );
    assert!(
        finalized.contains("internal reasoning") || finalized.contains("Reasoning (1 line)"),
        "reasoning content or collapsed header must be present; got:\n{finalized}"
    );
    // Assistant message must use the bullet, not the reasoning label.
    assert!(
        finalized.contains("• public reply"),
        "assistant must use bullet glyph; got:\n{finalized}"
    );
    // Reasoning cell must NOT use the bullet glyph for its content.
    assert!(
        !finalized.contains("• internal reasoning"),
        "reasoning text must not appear under a bullet glyph; got:\n{finalized}"
    );
}
