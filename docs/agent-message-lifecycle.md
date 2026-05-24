# Agent Message Cell Lifecycle (AM-01)

Codex is the source of truth for assistant message streaming. When nav's
current simpler behavior conflicts with Codex's behavior, follow Codex.

Tracked in [issue #219](https://github.com/season179/nav/issues/219).

> **Status**: AM-01 (design) and AM-02 (cell types) are **complete**.
> `AgentMessageCell`, `StreamingAgentTailCell`, and `AgentMarkdownCell` exist
> in `cells/messages.rs`. The controller rewiring (AM-03/AM-04) is pending.
> `AssistantStreamingCell` and the deprecated `AssistantMessageCell` alias
> remain until the widget migrates.

## Current nav shape

Today nav keeps the live assistant reply in one `AssistantStreamingCell`.
`AssistantMessageDelta` appends to that cell, the whole live message renders in
the inline viewport, and `AssistantMessageDone` converts the cell into one
`AgentMarkdownCell` for scrollback.

That behavior is documented in
[streaming-behavior.md](streaming-behavior.md). It is the baseline to replace,
not the target architecture.

## Codex target shape

Codex splits one assistant stream into three surfaces:

| Codex part | Target nav role |
|---|---|
| `AgentMessageCell` | A transient, already-rendered stable chunk emitted during streaming and written to scrollback. |
| `StreamingAgentTailCell` | The mutable active tail that remains in the nav-owned redraw viewport and is replaced on each delta. |
| `AgentMarkdownCell` | The finalized source-backed assistant message produced after consolidation. |
| `StreamController` | The owner of raw source, markdown/table holdback, stable chunk emission, and tail exposure. |

The target lifecycle is:

1. `AssistantMessageDelta` enters `StreamController`.
2. The controller partitions source into stable rendered lines and mutable tail
   lines.
3. Commit ticks dequeue stable lines and emit `AgentMessageCell` batches.
4. Emitted `AgentMessageCell`s enter nav's finalized/pending history path so
   they are written to terminal scrollback.
5. The mutable tail is rendered as `StreamingAgentTailCell` in the inline
   viewport and replaced whenever the tail changes.
6. `AssistantMessageDone` finalizes the controller, emits any remaining stable
   or tail content, preserves the raw markdown source, and clears the live tail.
7. A consolidation pass replaces the trailing run of `AgentMessageCell`s with
   one `AgentMarkdownCell` backed by the raw source.

## Nav behavior to replace

The migration should remove these current assumptions:

- `AssistantStreamingCell` is the single owner of both stable streamed text and
  mutable tail text.
- The whole assistant reply remains in the redraw viewport until
  `AssistantMessageDone`.
- Commit ticks only advance a visibility counter for a viewport-rendered cell.
- Finalization directly turns the whole streaming cell into `AgentMarkdownCell`
  without a transient `AgentMessageCell` run.
- `inline_lines_capped` is responsible for clipping the full live assistant
  reply. After the migration, it should only need to handle the active tail and
  live tool placeholders.

## Inline viewport and scrollback contract

The nav-owned redraw layer should stay small and mutable:

- `StreamingAgentTailCell` belongs in the inline viewport.
- Active tool placeholders and grouped running exploration rows belong in the
  inline viewport while they are still in flight.
- Stable `AgentMessageCell` chunks belong in terminal scrollback through the
  same pending-finalized drain path as other history cells.
- `AgentMarkdownCell` is the canonical finalized representation after
  consolidation.

Codex-style consolidation can require re-rendering the visible transcript after
the final tail is folded into the source-backed cell. If this conflicts with
nav's older "do not re-emit on resize" rule for assistant messages, the
assistant-message path should follow Codex and reflow only the affected stream
range needed for a correct final transcript.

## Mapping from current nav code

| Current nav code | Target action |
|---|---|
| `crates/nav-tui/src/cells/messages.rs::AssistantStreamingCell` | Split into Codex-style `AgentMessageCell` and `StreamingAgentTailCell`; keep any useful rendering helpers. |
| `crates/nav-tui/src/cells/messages.rs::AgentMarkdownCell` | Keep as the finalized cell, but ensure it is source-backed and used after consolidation. |
| `crates/nav-tui/src/streaming/controller.rs::StreamController` | Change from "render visible stable + tail" to "emit stable cells + expose current tail + return raw source on finalize". |
| `crates/nav-tui/src/chat.rs::streaming_assistant` | Replace with a controller-owned stream lifecycle plus an active tail cell. |
| `ChatWidget::on_commit_tick` | Drain controller output into pending history as `AgentMessageCell`s. |
| `ChatWidget::inline_lines` / `inline_lines_capped` | Render the active tail and live placeholders, not the full assistant reply. |
| `close_streaming_assistant_with` | Replace with finalization plus consolidation trigger. |

## Follow-up implementation sequence

1. [#220](https://github.com/season179/nav/issues/220) introduces the
   Codex-style cell split: `AgentMessageCell`, `StreamingAgentTailCell`, and
   finalized `AgentMarkdownCell`.
2. [#221](https://github.com/season179/nav/issues/221) changes
   `StreamController` so commit ticks emit stable `AgentMessageCell` chunks and
   expose only the mutable tail for active rendering.
3. [#222](https://github.com/season179/nav/issues/222) wires `ChatWidget` so
   stable chunks drain into scrollback while the tail stays live.
4. [#223](https://github.com/season179/nav/issues/223) cleans up names and docs
   after the split lands.
5. [#224](https://github.com/season179/nav/issues/224) locks down
   `AgentMessageCell` formatting snapshots.
6. [#225](https://github.com/season179/nav/issues/225) adds tmux-backed proof
   for the real stream handoff.
7. [#226](https://github.com/season179/nav/issues/226) implements
   Codex-style consolidation into one source-backed `AgentMarkdownCell`.

Any follow-up that verifies rendered TUI behavior must use tmux-backed proof
when tmux is available, and must skip cleanly with an explicit note when it is
not.
