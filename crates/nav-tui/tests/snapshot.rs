use nav_core::{
    AgentEvent, FileChangeKind, FileChangeSummary, FileDiffSummary, GitCheckpointAction,
    GitCheckpointStatus, PatchApplyStatus, PendingInputMode, ReviewDecision, SessionSummary,
    TurnUsage, UserAttachment,
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
fn tool_rows_label_skill_reads_and_truncate_known_outputs() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: json!({
            "path": "/Users/season/.agents/skills/zoom-out/SKILL.md"
        }),
    });
    // While the call is in flight, the placeholder lives inline and
    // shows the `Exploring` label — that row will be dropped (not
    // duplicated) the moment `ToolCallOutput` arrives.
    let started = render_widget(&mut widget, 90, 20);
    assert!(
        started.contains("• Exploring\n  Read SKILL.md (zoom-out skill)"),
        "{started}"
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

    let rendered = render_widget(&mut widget, 90, 20);

    assert!(
        !rendered.contains("• Exploring"),
        "in-flight placeholder must be replaced once the output lands; got:\n{rendered}"
    );
    assert!(
        rendered.contains("• Explored  Read SKILL.md (zoom-out skill)"),
        "{rendered}"
    );
    assert!(!rendered.contains("  └ 20 lines"), "{rendered}");
    assert!(!rendered.contains("line 03"), "{rendered}");
    assert!(!rendered.contains("… 16 more lines hidden"), "{rendered}");
    assert!(!rendered.contains("line 19"), "{rendered}");
}

#[test]
fn consecutive_same_action_explorations_collapse_into_one_line() {
    let mut widget = ChatWidget::new();

    // Two consecutive reads
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

    // A search (different action) stays separate
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

    let rendered = render_widget(&mut widget, 100, 20);

    // Two reads collapsed into one line
    assert!(
        rendered.contains("• Explored  Read docs/a.md, docs/b.md"),
        "{rendered}"
    );
    // Search stays as its own line
    assert!(
        rendered.contains("• Explored  Search \"inserthistory\" in src"),
        "{rendered}"
    );
    // No stats or preview for either
    assert!(!rendered.contains("  └"), "{rendered}");
    assert!(!rendered.contains("lines"), "{rendered}");
}

#[test]
fn explorations_merge_across_model_rounds_and_dedup_targets() {
    let mut widget = ChatWidget::new();

    // Round 1: read a.md and b.md
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
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "let me read more".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "let me read more".to_string(),
    });

    // Round 2: read b.md again (dedup) and c.md
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

    let rendered = render_widget(&mut widget, 100, 20);

    // All three reads merged; b.md appears only once
    assert!(
        rendered.contains("• Explored  Read a.md, b.md, c.md"),
        "expected merged reads with dedup; got:\n{rendered}"
    );
    // Only one Explored line, not three
    assert_eq!(
        rendered.matches("• Explored").count(),
        1,
        "expected exactly one Explored line; got:\n{rendered}"
    );
}

#[test]
fn exploration_collapse_flushes_on_bash_output() {
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
    // Non-exploration tool flushes the buffered exploration
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "b1".to_string(),
        name: "bash".to_string(),
        arguments: json!({ "command": "cargo test" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "b1".to_string(),
        output: "all tests passed".to_string(),
        is_error: false,
        truncation: None,
    });

    let rendered = render_widget(&mut widget, 100, 20);

    assert!(
        rendered.contains("• Explored  Read foo.rs"),
        "exploration must appear before bash output; got:\n{rendered}"
    );
    assert!(
        rendered.contains("• Ran  cargo test"),
        "{rendered}"
    );
    assert!(rendered.contains("  └ 1 line, 16 chars"), "{rendered}");
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

    widget.push_session_list(vec![
        SessionSummary {
            id: "01HZZZZZZZZZZZZZZZZZZZZZZZ".to_string(),
            name: Some("release work".to_string()),
            created_at: 100,
            updated_at: 250,
            last_active: 250,
            cwd: "/repo/nav".to_string(),
            provider: "openai-responses".to_string(),
            model: "gpt-test".to_string(),
            first_user_prompt: Some("Implement the resume picker".to_string()),
            tokens_input: 10,
            tokens_output: 5,
            tokens_input_cached: 0,
            tokens_reasoning: 0,
            cost_micros_reported: 0,
            turns_with_reported_cost: 0,
            turns_total: 2,
            turn_count: 2,
            cost_currency: "USD".to_string(),
            parent_id: None,
            labels: Vec::new(),
            child_count: 0,
        },
        SessionSummary {
            id: "01HYYYYYYYYYYYYYYYYYYYYYYYY".to_string(),
            name: None,
            created_at: 90,
            updated_at: 120,
            last_active: 120,
            cwd: "/repo/nav".to_string(),
            provider: "openai-responses".to_string(),
            model: "gpt-test".to_string(),
            first_user_prompt: None,
            tokens_input: 0,
            tokens_output: 0,
            tokens_input_cached: 0,
            tokens_reasoning: 0,
            cost_micros_reported: 0,
            turns_with_reported_cost: 0,
            turns_total: 0,
            turn_count: 0,
            cost_currency: "USD".to_string(),
            parent_id: None,
            labels: Vec::new(),
            child_count: 0,
        },
    ]);
    widget.push_session_notice("name", "Session name set to \"release work\"");
    widget.push_session_notice("export", "Wrote transcript to transcript.md");

    let rendered = render_widget(&mut widget, 96, 20);
    insta::assert_snapshot!("session_management_cells", rendered);
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
    // random strings, so lex order is effectively random. With a Vec the
    // display order matches issuance order.
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

    let rendered = render_widget(&mut widget, 80, 8);
    let first_idx = rendered.find("first.txt").expect("first placeholder");
    let second_idx = rendered.find("second.txt").expect("second placeholder");
    assert!(
        first_idx < second_idx,
        "arrival order (z_first, a_second) must beat lex order; got:\n{rendered}"
    );
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
