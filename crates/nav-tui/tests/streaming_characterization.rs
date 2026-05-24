//! Characterization tests for assistant streaming behavior (issue #217).
//!
//! These tests lock down the *current* semantics of how
//! `AssistantMessageDelta`, `AssistantMessageDone`, tool calls, and the
//! inline viewport cap interact. They are specification-grade: each test
//! reads as a statement of what the system does today. If the behavior
//! changes intentionally, the test should be updated to match.
//!
//! **No tmux tests** — the acceptance criteria are about event→cell
//! semantics, not rendered pixel output. tmux-backed proof is recorded
//! as explicitly skipped.

use nav_core::{AgentEvent, TurnUsage};
use nav_tui::ChatWidget;
use ratatui::text::Line;

fn lines_text(lines: &[Line<'_>]) -> String {
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            out.push_str(&span.content);
        }
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// AC-1: First `AssistantMessageDelta` starts an in-flight streaming cell
// ---------------------------------------------------------------------------

#[test]
fn first_delta_creates_streaming_cell_and_has_streaming_is_true() {
    let mut widget = ChatWidget::new();

    assert!(!widget.has_streaming(), "no streaming cell before any delta");

    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "hello".to_string(),
    });

    assert!(
        widget.has_streaming(),
        "first delta must create an in-flight streaming cell"
    );
    assert!(
        widget.streaming_height(80) > 0,
        "streaming cell must occupy at least one row"
    );

    let inline = lines_text(&widget.inline_lines(80));
    assert!(
        inline.contains("hello"),
        "streaming text must appear in inline viewport; got:\n{inline}"
    );
}

// ---------------------------------------------------------------------------
// AC-2: Multiple deltas render incrementally in the inline viewport
// ---------------------------------------------------------------------------

#[test]
fn multiple_deltas_accumulate_into_one_streaming_cell() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "Hello, ".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "world!".to_string(),
    });

    assert!(widget.has_streaming());
    let inline = lines_text(&widget.inline_lines(80));
    assert!(
        inline.contains("Hello, world!"),
        "deltas must concatenate inside one cell; got:\n{inline}"
    );
    assert_eq!(
        inline.matches("Hello,").count(),
        1,
        "text must not be duplicated across cells; got:\n{inline}"
    );
}

#[test]
fn commit_tick_releases_stable_lines_one_at_a_time_in_smooth_mode() {
    // Two source lines are pushed; the adaptive chunking policy starts in
    // smooth mode (queue depth < 8). Each commit tick releases exactly one
    // source line from the visibility gate. The live tail (partial line
    // without a trailing newline) always renders regardless of ticks.
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "alpha\nbeta\ngamma".to_string(),
    });

    // Before any tick, only the tail ("gamma") renders — the stable
    // lines are gated behind the commit-tick queue.
    let pre = lines_text(&widget.inline_lines(80));
    assert!(
        !pre.contains("alpha"),
        "stable line leaked before commit tick; got:\n{pre}"
    );
    assert!(
        pre.contains("gamma"),
        "live tail must always render; got:\n{pre}"
    );

    // First tick releases "alpha" (one source line) into pending history.
    let advanced = widget.on_commit_tick(80);
    assert!(advanced, "tick must report progress");
    let after_one = lines_text(&widget.drain_pending(80));
    assert!(
        after_one.contains("alpha"),
        "first tick must release the first stable line; got:\n{after_one}"
    );
    assert!(
        !after_one.contains("beta"),
        "second stable line must stay gated after one tick; got:\n{after_one}"
    );

    // Second tick releases "beta".
    widget.on_commit_tick(80);
    let after_two = lines_text(&widget.drain_pending(80));
    assert!(
        after_two.contains("beta"),
        "second tick must release the second stable line; got:\n{after_two}"
    );
}

// ---------------------------------------------------------------------------
// AC-3: `AssistantMessageDone` finalizes into `AgentMarkdownCell`
// ---------------------------------------------------------------------------

#[test]
fn done_after_deltas_replaces_buffer_and_moves_to_scrollback() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "partial ".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "chunk".to_string(),
    });

    assert!(
        widget.has_streaming(),
        "streaming cell must be live before Done"
    );

    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "Finalized assistant reply text".to_string(),
    });

    // The streaming cell is gone — finalized into scrollback.
    assert!(
        !widget.has_streaming(),
        "AssistantMessageDone must close the streaming cell"
    );

    // The coalesced text lands in scrollback (drain_pending), not inline.
    let scrollback = lines_text(&widget.drain_pending(80));
    assert!(
        scrollback.contains("Finalized assistant reply text"),
        "Done text must replace the delta buffer; got:\n{scrollback}"
    );
    assert!(
        !scrollback.contains("partial"),
        "delta-level text must not leak alongside the coalesced Done text; got:\n{scrollback}"
    );

    // Inline viewport must be empty now.
    assert_eq!(
        widget.inline_lines(80).len(),
        0,
        "no live tail after finalization"
    );
}

#[test]
fn done_before_any_commit_tick_stays_source_backed_for_reflow() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "partial text".to_string(),
    });
    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "Finalized assistant reply text wraps after completion".to_string(),
    });

    let scrollback = lines_text(&widget.drain_pending(24));
    assert!(
        scrollback.contains("• Finalized assistant"),
        "finalized source should render with assistant chrome; got:\n{scrollback}"
    );
    assert!(
        scrollback.contains("  reply text wraps"),
        "finalized source should rewrap at drain width; got:\n{scrollback}"
    );
}

#[test]
fn done_without_any_prior_delta_creates_markdown_cell_directly() {
    // Resume path: the session store replays AssistantMessageDone without
    // ever seeing a Delta. The widget must still produce a finalized cell.
    let mut widget = ChatWidget::new();

    assert!(!widget.has_streaming());

    widget.ingest(AgentEvent::AssistantMessageDone {
        text: "resumed text".to_string(),
    });

    assert!(!widget.has_streaming(), "no streaming cell was created");
    let scrollback = lines_text(&widget.drain_pending(80));
    assert!(
        scrollback.contains("resumed text"),
        "Done without Delta must still produce a finalized cell; got:\n{scrollback}"
    );
}

// ---------------------------------------------------------------------------
// AC-4: Tool calls close or coexist with streaming
// ---------------------------------------------------------------------------

#[test]
fn tool_call_started_finalizes_open_streaming_cell() {
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "thinking...".to_string(),
    });
    assert!(widget.has_streaming());

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "bash".to_string(),
        arguments: serde_json::json!({ "command": "ls" }),
    });

    // The streaming cell must be closed and finalized into scrollback.
    assert!(
        !widget.has_streaming(),
        "ToolCallStarted must finalize the in-flight streaming cell"
    );

    let scrollback = lines_text(&widget.drain_pending(80));
    assert!(
        scrollback.contains("thinking..."),
        "partial streaming text must survive in scrollback; got:\n{scrollback}"
    );

    // The tool placeholder lives inline, not in scrollback.
    let inline = lines_text(&widget.inline_lines(80));
    assert!(
        inline.contains("bash") || inline.contains("ls"),
        "tool placeholder must render inline; got:\n{inline}"
    );
}

#[test]
fn new_delta_after_tool_call_starts_a_fresh_streaming_cell() {
    // The model emits: delta → tool → delta → done.
    // Each tool call closes the current streaming cell, so the second
    // delta opens a brand-new one.
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "first reply".to_string(),
    });
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "bash".to_string(),
        arguments: serde_json::json!({ "command": "ls" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "call_1".to_string(),
        output: "file.txt".to_string(),
        is_error: false,
        truncation: None,
    });
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "second reply".to_string(),
    });

    assert!(widget.has_streaming());
    let inline = lines_text(&widget.inline_lines(80));
    assert!(
        inline.contains("second reply"),
        "new delta after tool must start a fresh streaming cell; got:\n{inline}"
    );
    assert!(
        !inline.contains("first reply"),
        "previous streaming cell must not leak into the new one; got:\n{inline}"
    );
}

#[test]
fn read_only_tool_call_collapses_into_exploring_group_not_separate_row() {
    // Read-only tools (read_file, code_search, list_files) buffer into
    // an exploring group instead of producing per-call scrollback rows.
    let mut widget = ChatWidget::new();

    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "r1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({ "path": "a.rs" }),
    });
    widget.ingest(AgentEvent::ToolCallOutput {
        call_id: "r1".to_string(),
        output: "content".to_string(),
        is_error: false,
        truncation: None,
    });

    // The read call is buffered, not yet in scrollback.
    let inline = lines_text(&widget.inline_lines(80));
    assert!(
        inline.contains("Exploring"),
        "read-only call must show as exploring group inline; got:\n{inline}"
    );

    // Turn end flushes the group to scrollback.
    widget.ingest(AgentEvent::TurnComplete {
        usage: nav_core::TurnUsage::default(),
    });
    let scrollback = lines_text(&widget.drain_pending(80));
    assert!(
        scrollback.contains("Exploring (1 call)"),
        "turn end must flush exploring group to scrollback; got:\n{scrollback}"
    );
}

// ---------------------------------------------------------------------------
// AC-5: Long streaming output is capped by `inline_lines_capped`
//
// The cap is applied by the viewport renderer (inline_region + render.rs).
// `inline_lines_capped` head-clips streaming text so tool-call placeholders
// (Exploring/Running rows) remain visible even when the assistant reply is
// long. ToolCallStarted closes the streaming cell, so the real cap scenario
// is: a long streaming cell that would overflow the viewport, with tool
// placeholders arriving *after* the cell is finalized (at which point only
// placeholders are inline).
// ---------------------------------------------------------------------------

#[test]
fn inline_lines_capped_head_clips_streaming_to_preserve_tool_placeholders() {
    // When the streaming cell is large and a read-only tool call is in
    // flight, `ToolCallStarted` finalizes the streaming cell into scrollback.
    // The tool placeholder lives inline below. If the placeholder were
    // past the cap, ratatui would silently clip it — so the cap must
    // keep placeholders visible.
    let mut widget = ChatWidget::new();

    // Build a large streaming cell then finalize it with a tool call.
    let long_text: String = (0..40)
        .map(|i| format!("line {:02}\n", i))
        .collect();
    widget.ingest(AgentEvent::AssistantMessageDelta { text: long_text });
    widget.on_commit_tick(80); // catch-up: release all stable lines

    // ToolCallStarted closes the streaming cell (it goes to scrollback)
    // and the placeholder renders inline instead.
    widget.ingest(AgentEvent::ToolCallStarted {
        call_id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({ "path": "/visible.txt" }),
    });

    // At this point the streaming cell is finalized; only the placeholder
    // is inline. The cap must not clip it.
    assert!(!widget.has_streaming(), "tool call must close streaming cell");

    let cap = 8u16;
    let capped = widget.inline_lines_capped(80, cap);
    assert!(
        capped.len() <= cap as usize,
        "capped output must not exceed the row cap; got {}",
        capped.len()
    );

    let capped_text = lines_text(&capped);
    assert!(
        capped_text.contains("visible.txt"),
        "tool placeholder must survive the cap; got:\n{capped_text}"
    );
}

#[test]
fn inline_lines_capped_returns_everything_when_under_cap() {
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "short".to_string(),
    });

    let capped = widget.inline_lines_capped(80, 20);
    let uncapped = widget.inline_lines(80);
    assert_eq!(
        capped.len(),
        uncapped.len(),
        "under the cap, capped and uncapped must be identical"
    );
}

#[test]
fn inline_lines_capped_clamps_long_streaming_to_cap() {
    // Stable chunks leave the live viewport as commit ticks run, so a long
    // stream must not make inline rendering grow past the cap.
    let mut widget = ChatWidget::new();
    let long_text: String = (0..30)
        .map(|i| format!("line {:02}\n", i))
        .collect();
    widget.ingest(AgentEvent::AssistantMessageDelta { text: long_text });
    widget.on_commit_tick(80); // catch-up: release all stable lines

    let cap = 6u16;
    let capped = widget.inline_lines_capped(80, cap);
    assert!(
        capped.len() <= cap as usize,
        "long streaming must stay within the cap; got {}",
        capped.len()
    );

    let capped_text = lines_text(&capped);
    assert!(
        !capped_text.contains("line 00"),
        "stable scrollback chunks must not remain in the live viewport; got:\n{capped_text}"
    );
    let scrollback = lines_text(&widget.drain_pending(80));
    assert!(
        scrollback.contains("line 29"),
        "newest stable lines should drain to pending history; got:\n{scrollback}"
    );
}

// ---------------------------------------------------------------------------
// AC-6 (supplementary): TurnComplete and end-of-turn semantics
//
// Event durability (Delta=transient, Done=durable) is already covered by
// `nav_core::agent_loop::events::tests`. This section characterizes how
// TurnComplete interacts with the streaming cell in the widget.
// ---------------------------------------------------------------------------

#[test]
fn turn_complete_closes_streaming_cell_and_drains_inflight() {
    // TurnComplete is emitted after each tool-call iteration (it acts as
    // a replay anchor). It closes the streaming cell via
    // `end_active_turn_viewport`, which also drains inflight tool
    // placeholders. The finalized text lands in scrollback.
    let mut widget = ChatWidget::new();
    widget.ingest(AgentEvent::AssistantMessageDelta {
        text: "streaming".to_string(),
    });
    widget.ingest(AgentEvent::TurnComplete {
        usage: TurnUsage::default(),
    });

    assert!(!widget.has_streaming(), "TurnComplete must close the streaming cell");
    let scrollback = lines_text(&widget.drain_pending(80));
    assert!(
        scrollback.contains("streaming"),
        "partial text must survive in scrollback after TurnComplete; got:\n{scrollback}"
    );
}
