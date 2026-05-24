# Assistant Streaming Behavior (AM-00)

Characterization of nav's current assistant streaming pipeline before
changing it toward Codex parity.

> **Scope**: read-only documentation and focused tests. No behavior changes.
> Tracked in [issue #217](https://github.com/season179/nav/issues/217).

## Event Flow

```
SSE stream                  AgentEvent                 ChatWidget cell
─────────────────────────────────────────────────────────────────────
response.output_text.delta → AssistantMessageDelta ──→ creates/reuses
                            (transient)                AssistantStreamingCell
                                                       (inline viewport)

response.output_item.done → AssistantMessageDone ──→ replace_buffer(),
(message type)             (durable)                 finalize(), push
                                                     AgentMarkdownCell
                                                     to scrollback
```

## Key Behaviors

### 1. `AssistantMessageDelta` starts an in-flight cell

The first `AssistantMessageDelta` received by `ChatWidget::ingest` creates
an `AssistantStreamingCell` held in `streaming_assistant`. Subsequent deltas
append text to the same cell — there is never more than one in-flight
streaming cell. `has_streaming()` returns `true` while it exists.

### 2. Multiple deltas render incrementally

`StreamController` accumulates raw markdown text. The controller's
**partition** splits content into a *stable* region (complete lines outside
open fences/tables) and a *live tail* (incomplete lines, open fences, open
tables).

- The **live tail** always renders in the viewport — the user sees text grow
  in real time.
- The **stable region** is gated behind a visibility counter
  (`visible_stable_lines`). Each commit tick releases one source line in
  *smooth mode*, or many in *catch-up mode* (queue depth ≥ 8 or oldest line
  age ≥ 120ms).

### 3. `AssistantMessageDone` finalizes into `AgentMarkdownCell`

`AssistantMessageDone` calls `close_streaming_assistant_with(text)`, which:

1. Replaces the stream buffer with the provider's coalesced `text`
   (`replace_buffer` → `finalize`).
2. Converts the `AssistantStreamingCell` into an `AgentMarkdownCell`.
3. Pushes it to the `finalized` vec (scrollback).
4. The inline viewport is now empty (no streaming cell, no live tail).

When `Done` arrives without a prior `Delta` (resume path), a new
`AgentMarkdownCell` is created directly.

### 4. Tool calls close the streaming cell

`ToolCallStarted` closes any in-flight streaming cell *before* creating the
tool placeholder. The partial text is finalized into scrollback. The tool
placeholder renders in the inline viewport below.

When the next `AssistantMessageDelta` arrives, a *new* streaming cell is
created — each tool-call iteration produces a distinct streaming segment.

Read-only tool outputs (`read_file`, `code_search`, `list_files`) are
buffered into an **exploring group** instead of per-call scrollback rows.
The group flushes to scrollback when a new streaming cell starts, a
non-read-only tool starts, or the turn ends.

### 5. Long streaming output is capped

`ChatWidget::inline_lines_capped(width, max_rows)` head-clips the streaming
cell so tool-call placeholders remain visible within the viewport budget.
`MAX_STREAMING_ROWS` (16) caps the streaming preview at the viewport level;
`render.rs` materializes lines through this cap before handing them to
ratatui's `Paragraph`.

The cap prioritizes placeholders over streaming text: if placeholders alone
fill the cap, streaming text is dropped entirely.

### 6. `TurnComplete` closes the streaming cell

`TurnComplete` is emitted after every tool-call iteration (it acts as a
replay anchor, not just a final turn signal). It calls
`end_active_turn_viewport()`, which closes any in-flight streaming cell and
drains inflight tool placeholders into scrollback (folding read-only ones
into the exploring group).

### 7. Durability

| Event | Durable | Persisted to session log |
|-------|---------|------------------------|
| `AssistantMessageDelta` | ✗ | Transient — live rendering only |
| `AssistantMessageDone` | ✓ | Canonical record for replay |
| `ReasoningDelta` | ✗ | Transient |
| `ReasoningDone` | ✗ | Transient — encrypted handle in `ResponseContinuation` is the durable record |

## Architecture

```
nav-core (agent_loop)
  runner.rs: emit_stream_events()
    SSE "response.output_text.delta" → AgentEvent::AssistantMessageDelta
    SSE "response.output_item.done"  → AgentEvent::AssistantMessageDone

  events.rs: AgentEvent enum, is_durable(), kind()

nav-tui
  chat.rs: ChatWidget::ingest()
    Delta → create/append AssistantStreamingCell (inline)
    Done  → close_streaming_assistant_with() → AgentMarkdownCell (scrollback)

  streaming/controller.rs: StreamController
    push_delta(), finalize(), visible_lines()
    Partition: stable vs tail (fences, tables)
    Visibility gate: visible_stable_lines (released by commit ticks)

  streaming/chunking.rs: AdaptiveChunkingPolicy
    Smooth mode (default): 1 line/tick
    Catch-up mode: batch drain (queue ≥ 8 lines or age ≥ 120ms)

  app/inline_region.rs: MAX_STREAMING_ROWS = 16
    streaming_cap() → limits inline_lines_capped budget

  app/render.rs: draw_tui()
    Materializes capped lines, splits viewport into streaming + pane chunks
```

## Tests

Focused characterization tests live in:
- `crates/nav-tui/tests/streaming_characterization.rs` — 12 tests covering
  all five acceptance criteria plus TurnComplete semantics.
- `crates/nav-tui/tests/snapshot.rs` — existing integration tests that also
  exercise streaming behavior at a higher level.
- `crates/nav-tui/src/cells/messages.rs` — unit tests for
  `AssistantStreamingCell` and `AgentMarkdownCell`.
- `crates/nav-tui/src/streaming/controller.rs` — partition, visibility gate,
  and snap-on-advance tests.
- `crates/nav-core/src/agent_loop/events.rs` — wire-format and
  durability tests (`is_durable`, `kind()` round-trips).

tmux-backed proof is explicitly **skipped** — these acceptance criteria are
about event→cell semantics, not rendered pixel output.
