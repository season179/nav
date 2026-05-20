# Pre-Compaction Token Efficiency PRD

## Problem Statement

nav reaches high model-visible token counts too quickly during ordinary turns, before long-session compaction is relevant. The practical symptom is that a user can hit large context numbers in fewer turns than they do with sibling agents such as `../pi`, `../opencode`, and `../kimiflare`.

This PRD deliberately sets compaction aside. The target is the request body nav sends for each normal turn before a `/compact` or automatic compaction run:

- what history is replayed between turns
- how much old tool output remains model-visible
- whether the model can see enough prior tool context to avoid repeating work
- whether file and search tools encourage targeted reads instead of broad payloads

The current highest-confidence issue is replay fidelity. In TUI mode every new turn reloads the persisted session and calls `rebuild_responses_input(...)`, but replay currently skips `ToolCallStarted` and `ToolCallOutput`. nav therefore persists tool events for scrollback, then removes them from the model-visible history on the next normal turn. The model sees user and assistant prose, but not the concrete tool call/result chain that produced that prose.

That behavior saves tokens in a crude way, but it also makes the model more likely to re-read files, re-run searches, or reconstruct state from assistant text. The fix is not to dump all raw tool output back into every turn. The fix is to replay the right tool context under a budget.

## Reference Behavior

### pi

`../pi` makes targeted context a first-class tool behavior before compaction:

- `read` supports `offset` and `limit`, and its tool description tells the model to continue with offsets for large files.
- `grep`, `find`, and `ls` expose limits and return actionable truncation notices.
- `bash` output is bounded and stores full output separately when truncated.

The useful lesson for nav is not "copy pi compaction"; it is that ordinary tool use should lead the model toward smaller follow-up reads.

### opencode

`../opencode` has a pre-compaction pruning mechanism for old tool results:

- completed tool parts can be marked compacted
- replay renders compacted old outputs as `[Old tool result content cleared]`
- recent/protected context is kept, while stale large outputs are cleared before they inflate future turns

The useful lesson for nav is that old tool results should become explicit placeholders, not disappear entirely and not stay raw forever.

### kimiflare

`../kimiflare` reduces large tool outputs at the tool boundary and stores raw output behind artifacts:

- reduced outputs include an artifact id
- `expand_artifact` retrieves the full raw output when needed
- read/grep/bash/web fetch tools are designed around reduced summaries plus targeted expansion

The useful lesson for nav is that the model can act on a compact view if the full output remains recoverable through an explicit path.

## Goals

1. Keep normal-turn replay faithful enough that the model knows which tools were called and what they returned.
2. Bound normal-turn replay so old tool outputs do not accumulate unboundedly before compaction.
3. Preserve recent, high-signal tool output verbatim.
4. Replace stale tool output with explicit placeholders instead of dropping the tool exchange.
5. Make targeted file reads available through nav's native `read_file` tool, not only through rewritten shell `cat` / `head` / `tail`.
6. Preserve local-first, `store: false` Responses behavior.
7. Keep scrollback and exports truthful: UI history can show full durable events even when model replay is budgeted.
8. Make `/context` report the same budgeted replay shape the next normal turn will actually send.

## Non-Goals

- Do not change `/compact`, compaction prompts, compaction summaries, or auto-compaction thresholds.
- Do not add memory, cross-session recall, or LLM-generated state packets.
- Do not make server-side Responses storage required.
- Do not hide recent tool failures that are likely needed for immediate debugging.
- Do not redesign the TUI.

## Proposed Solution

Add a pre-compaction replay policy layer between the persisted `AgentEvent` log and the Responses `input` array.

Today:

```text
session events -> rebuild_responses_input -> user/assistant messages only
```

Target:

```text
session events -> rebuild_responses_input_with_policy -> budgeted Responses input
```

The budgeted input should include:

1. User messages and assistant messages, as today.
2. Provider continuation items required by Responses `store: false`, especially encrypted reasoning items when present.
3. Function-call items for prior tool calls.
4. Function-call-output items for recent or budget-protected tool outputs.
5. Placeholder outputs for older tool results that are no longer worth replaying raw.

A placeholder should be explicit and stable, for example:

```text
[Old tool result content cleared; original output is available in session log]
```

If nav adds artifact-backed full-output retrieval later, the placeholder can include an artifact id. That should be a second slice; the first slice can rely on the durable session log and focused re-runs.

## Replay Policy

The first implementation should use deterministic rules:

- Keep the last `N` user turns' tool outputs verbatim. Start with `N = 2`.
- Keep all tool outputs after the latest user message verbatim while a turn is active.
- Keep outputs from failed tools verbatim for at least the last `N` user turns.
- Keep mutation tool outputs and file-change metadata visible in compact form.
- Replace older successful read/search/bash outputs with placeholders.
- Preserve the matching `function_call` item whenever a `function_call_output` placeholder is emitted, so the transcript remains structurally valid.
- Never replay a `function_call_output` without its matching `function_call`.
- Never replay encrypted reasoning detached from the model output item it is meant to continue.

The initial constants should live in one policy module, not scattered through replay:

```rust
pub struct ReplayBudget {
    pub raw_tool_turns: usize,
    pub max_raw_tool_output_bytes: usize,
    pub max_total_tool_output_bytes: usize,
}
```

Suggested defaults:

- `raw_tool_turns = 2`
- `max_raw_tool_output_bytes = 50 * 1024`
- `max_total_tool_output_bytes = 120 * 1024`

These are deliberately conservative. They should be easy to tune after `/context all` shows real distributions.

## Provider Continuation Items

nav already requests `reasoning.encrypted_content` from the Responses API. With `store: false`, that encrypted reasoning is the provider-safe way to continue tool-call conversations without server-side state.

Add durable support for model output items that are needed for replay:

- Decode `response.output_item.done` items of type `reasoning`.
- Persist a durable event for replayable provider output items, or add a dedicated `AgentEvent::Reasoning` if the narrower shape is sufficient.
- Do not persist plaintext hidden reasoning. Store only provider-returned encrypted continuation content and any public summary fields needed by the API.
- Keep existing user-facing `AssistantMessageDone` behavior for visible assistant text.

The replay layer can then reconstruct the Responses `input` array without relying on the in-memory `raw_output` vector that exists only inside one `run_agent` call.

## Native Read Ergonomics

nav's `read_file` currently accepts only `path`. That encourages broad reads, especially for large files. Shell read rewriting helps when the model chooses `cat`, `head`, or `tail`, but the native tool should be efficient on its own.

Extend `read_file` to support:

```json
{
  "path": "src/lib.rs",
  "offset": 1,
  "limit": 200
}
```

Behavior:

- `offset` is 1-indexed.
- `limit` is a maximum number of lines.
- When output is truncated because of default line/byte caps, append the next offset.
- When a user-provided `limit` stops before EOF, append a remaining-lines notice.
- Preserve existing path-security, protected-read, and skill-dir behavior.

This should reduce pre-compaction growth because the model has a normal, safe way to inspect file slices rather than pulling full files or falling back to shell.

## User Stories

1. As a daily nav user, I want the agent to remember which files it already read in the previous turn, so it does not re-read them just because a new prompt started.
2. As a daily nav user, I want recent tool outputs to remain visible to the model, so immediate debugging context is not lost.
3. As a daily nav user, I want old bulky tool outputs to stop inflating every normal turn, so I have more useful work before compaction is needed.
4. As a daily nav user, I want stale tool outputs to become clear placeholders, so the model knows a tool ran even when old content is cleared.
5. As a daily nav user, I want failed commands and recent errors preserved longer than successful reads, so nav can fix the current problem without rerunning everything.
6. As a daily nav user, I want `read_file` to support `offset` and `limit`, so the model can inspect a specific file slice.
7. As a daily nav user, I want truncation messages to tell the model how to continue, so it can choose the next narrow read.
8. As a daily nav user, I want `/context all` to explain which tool outputs are raw vs cleared, so token growth is inspectable.
9. As an NDJSON consumer, I want the durable event log to remain truthful, so UI history and exports do not depend on model replay pruning.
10. As a maintainer, I want replay budgeting tested independently from model calls, so token policy changes do not require live API tests.

## Implementation Plan

### 1. Add Replay Policy Types

Add a small module near replay, for example:

```text
crates/nav-core/src/agent/replay_policy.rs
```

Responsibilities:

- group durable events into user turns
- decide which tool outputs stay raw
- decide which tool outputs become placeholders
- enforce total raw tool output budget
- expose deterministic helper functions for tests

Keep `replay.rs` responsible for translating selected events into Responses `Value` items.

### 2. Persist Replayable Provider Output

Update response collection and event emission:

- Extend `ResponseItem` for reasoning output items.
- Capture replayable raw output items from `response.output_item.done`.
- Add a durable event shape for provider continuation items.
- Persist those events before tool execution emits `ToolCallStarted` / `ToolCallOutput`.

This is the piece that makes `store: false` replay correct across TUI turns, resumed sessions, and headless `--resume`.

### 3. Replay Tool Calls Under Budget

Update `rebuild_responses_input` to include tool exchanges:

- `ToolCallStarted` -> `{"type":"function_call", ...}`
- `ToolCallOutput` -> `{"type":"function_call_output", ...}`
- old outputs -> placeholder `function_call_output`
- provider continuation event -> original provider item

Retain the aborted-turn truncation behavior: if a turn is aborted, remove partial replay for that turn.

### 4. Extend Context Reporting

Update `context_report.rs` so `/context` and `/context all` measure the budgeted replay, not an idealized user/assistant-only replay.

Add item labels such as:

- `tool output 3 (raw)`
- `tool output 4 (cleared)`
- `reasoning continuation`

This makes the PR auditable without needing live API calls.

### 5. Add Native `read_file` Slicing

Update:

- `tool_definitions`
- `run_tool` argument parsing
- `fs::read_file` or a new `fs::read_file_slice`
- tests for path safety plus offset/limit behavior

The shell `cat` / `head` / `tail` rewrite can stay as an additional ergonomic path.

## Testing Decisions

Add focused tests before touching broad integration:

- `rebuild_responses_input_replays_tool_call_and_output_for_recent_turn`
- `rebuild_responses_input_clears_old_tool_output_with_placeholder`
- `rebuild_responses_input_never_emits_output_without_matching_call`
- `rebuild_responses_input_preserves_failed_recent_tool_output`
- `rebuild_responses_input_drops_aborted_turn_tool_context`
- `rebuild_responses_input_preserves_provider_reasoning_continuation`
- `context_report_counts_raw_and_cleared_tool_outputs`
- `read_file_supports_offset_and_limit`
- `read_file_reports_next_offset_when_truncated`
- `read_file_offset_limit_preserves_protected_read_rules`

Run at minimum:

```text
cargo test -p nav-core replay
cargo test -p nav-core context_report
cargo test -p nav-core tool_registry::
cargo test -p nav-tui
```

Before landing, run the broader gate:

```text
cargo test -p nav-core -p nav-tui
cargo clippy --workspace --all-targets -- -D warnings
```

## Acceptance Criteria

- A normal TUI turn after a tool-using turn replays the prior tool call and a budgeted result or placeholder.
- Recent successful tool outputs remain raw.
- Older successful bulky tool outputs become placeholders before compaction.
- Failed recent tool outputs remain raw.
- Responses continuation items needed for `store: false` replay are persisted durably.
- `/context all` shows raw vs cleared tool-output entries.
- `read_file` supports `offset` and `limit`.
- Existing compacted-session replay still starts from the latest checkpoint, but post-checkpoint normal turns use the same pre-compaction replay policy.
- No compaction prompt, summary format, or threshold behavior changes in this PR.

## Open Questions

- Should replay budgeting be configurable in `.nav/settings.json` immediately, or kept internal until defaults are proven?
- Should placeholders include a session event sequence number so a future `expand_event_output` tool can recover the exact old output?
- Should mutation outputs be replayed raw, or should `FileChange` summaries become the model-visible source of truth for mutations?
- Should `read_file` default to a smaller line window than the global `50 KB / 2000 lines` cap once offset/limit exists?

## Review Notes

This work is the natural predecessor to any further compaction work. If ordinary-turn replay is wrong or bloated, compaction will only hide the symptom later. The first valuable slice is replay fidelity plus bounded old-output placeholders; native read slicing is the second slice if we want the model to generate less bulky context in the first place.
