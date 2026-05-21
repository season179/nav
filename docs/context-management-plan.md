# Context Management Plan

Combines sibling-agent comparisons, the Amp guide, and one prior PRD into a
single ranked roadmap:

- **Sibling-agent comparisons** — what `../pi`, `../opencode`, `../codex`, and
  `../kimiflare` do turn-by-turn that nav lacks.
- **Amp comparison** — Amp Context Management guide (https://ampcode.com/guides/context-management) vs. nav.
- **Prior PRD** — the pre-compaction replay-fidelity work, now folded in as the detailed design for the highest-leverage Tier 1 items.

Ranked by ROI against the actual symptom: **nav reaches the compaction threshold in ~2 turns**.

## Problem Statement

nav reaches high model-visible token counts too quickly during ordinary turns, before long-session compaction is relevant. A user can hit large context numbers in fewer turns than they do with sibling agents (`../pi`, `../opencode`, `../kimiflare`, Amp).

This plan deliberately sets compaction aside. The target is the request body nav sends for each normal turn *before* a `/compact` or auto-compaction run:

- what fixed material (system prompt, skills, tool defs, project context) ships every turn
- what history is replayed between turns
- how much old tool output remains model-visible
- whether the model can see enough prior tool context to avoid repeating work
- whether file and search tools encourage targeted reads instead of broad payloads

A specific replay bug compounds the budget problem: in TUI mode every new turn reloads the persisted session and calls `rebuild_responses_input(...)`, but replay currently skips `ToolCallStarted` and `ToolCallOutput`. nav persists tool events for scrollback, then removes them from the model-visible history on the next turn. The model sees user and assistant prose, but not the concrete tool call/result chain that produced that prose. That saves tokens crudely while making the model more likely to re-read files, re-run searches, or reconstruct state from assistant text.

## Reference Behavior

### pi
- `read` supports `offset` / `limit`; its tool description tells the model to continue with offsets on large files.
- `grep`, `find`, `ls` expose limits and return actionable truncation notices.
- `bash` output is bounded; full output stored separately when truncated (spillover to a temp file above 100KB).
- Per-message `excludeFromContext` flag — the session log keeps the message while the LLM input filters it out.
- Compaction uses `keepRecentTokens` to retain a token budget of recent work.

### opencode
- Tool truncation writes the full output to a truncation directory, returns a
  preview, and records an `outputPath`.
- `read` supports `offset` / `limit` for both files and directories, with
  continuation hints.
- Completed tool parts can be marked compacted.
- Replay renders compacted old outputs as `[Old tool result content cleared]`.
- Recent/protected context kept; stale large outputs cleared before they inflate future turns.
- Overflow checks reserve output/headroom tokens before deciding the usable input budget.

### codex
- A `ContextManager` owns model-visible history instead of letting the loop
  append raw JSON forever.
- History normalization enforces call/output pairing, removes orphan outputs,
  and strips image content when the target model does not support images.
- Function/custom tool outputs are truncated by `TruncationPolicy` at record
  time, including token-based budgets.
- Token accounting separates the last API response total from estimated
  model-visible bytes/tokens added since the last successful response.
- Websocket transport can send incremental input deltas when a request strictly
  extends the previous one, avoiding repeated shipment of unchanged history.
- Environment, permissions, realtime, model, and personality context are emitted
  as diffs when they change instead of blindly re-injecting everything.

### kimiflare
- Reduced outputs include an artifact id.
- `expand_artifact` retrieves the full raw output when needed.
- read/grep/bash/web fetch tools are designed around reduced summaries plus targeted expansion.
- `read` supports `offset` / `limit`; full-file reads reduce to a compact
  outline by default.
- Historical assistant reasoning can be stripped while preserving the latest
  assistant reasoning.
- Older user images can be removed from the API message array.
- Tool-call budget reminders push the model to stop broad exploration before a
  turn balloons.

### Amp
- Submit-time `@<filename>` scanning resolves and inlines files at message send.
- File-mention truncation policy: 500 lines / 2KB per line.
- Ambient context: OS, cwd listing, open file, selected text.
- Edit / restore-to-earlier-message rewrites the current thread.
- Handoff: second-model extraction of relevant messages, tool calls, and files into a draft prompt for a fresh thread.
- `read_thread` tool to reference other threads by ID/URL.

The useful lessons:

- **From pi**: ordinary tool use should lead the model toward smaller follow-up reads; per-event exclusion is cheaper than re-summarizing.
- **From opencode**: old tool results should become explicit placeholders, not disappear and not stay raw forever; saved full outputs make truncation recoverable.
- **From codex**: model-visible history needs a manager with normalization,
  modality-aware stripping, token-policy truncation, and request-delta support.
- **From kimiflare**: the model can act on a compact view if the full output remains recoverable through an explicit path; semantic reducers are stronger than generic truncation.
- **From Amp**: caching + lazy ambient + thread-level rewinds reduce both per-turn bytes and "let me re-read that" round-trips.

## Goals

1. Keep normal-turn replay faithful enough that the model knows which tools were called and what they returned.
2. Bound normal-turn replay so old tool outputs do not accumulate unboundedly before compaction.
3. Preserve recent, high-signal tool output verbatim.
4. Replace stale tool output with explicit placeholders instead of dropping the tool exchange.
5. Cut fixed per-turn overhead from the system prompt and tool definitions.
6. Make targeted file reads available through nav's native `read_file` tool, not only through rewritten shell `cat` / `head` / `tail`.
7. Preserve local-first, `store: false` Responses behavior.
8. Keep scrollback and exports truthful: UI history can show full durable events even when model replay is budgeted.
9. Make `/context` report the same budgeted replay shape the next normal turn will actually send.
10. Strip or replace low-value historical modalities (old images, old reasoning
    continuation/narration) before they become compaction pressure.
11. Add loop-level backpressure so one broad tool-using turn cannot consume the
    next two turns' context budget.

## Non-Goals

- Do not change `/compact`, compaction prompts, compaction summaries, or auto-compaction thresholds.
- Do not add memory, cross-session recall, or LLM-generated state packets.
- Do not make server-side Responses storage required.
- Do not hide recent tool failures that are likely needed for immediate debugging.
- Do not redesign the TUI.

## Ranked Roadmap

Tier 1 directly attacks the 2-turn blowup. Tier 2 is UX work that does not move per-turn token cost.

### Tier 1 — Per-turn payload reduction

#### 1. Lazy skills section in system prompt
- **Where**: `crates/nav-core/src/context/mod.rs:81-112` (`skill_instruction_section`).
- **Problem**: every skill's name + description + SKILL.md path + skill_dir is inlined every turn. With a large catalog this is plausibly the single largest fixed per-turn cost.
- **Fix**: ship only `name` + 1-line summary. The model loads SKILL.md via `read_file` when it picks one.
- **Risk**: low. Tool already supports reading SKILL.md by path.

#### 2. Cache instructions + tool definitions across turns
- **Where**: `crates/nav-core/src/model/responses/request.rs:55-76` (instructions) and `crates/nav-core/src/tool_registry/definitions.rs:17` (tool defs).
- **Problem**: `build_instructions()` (cwd + skills + every project context file body) and `tool_definitions()` JSON are byte-identical across most turns, but re-billed.
- **Fix**: use provider prompt-caching markers so these two blocks hit the cache after turn 1.
- **Risk**: low. No behavior change.

#### 3. Proactive token-budget pruning before each request
- **Where**: `crates/nav-core/src/agent_loop/runner.rs:797-830` (`drop_oldest_tool_pair`).
- **Problem**: pruning only fires reactively, after the API returns `ContextWindowExceeded`. The oversize turn is already paid for and may have failed mid-stream.
- **Fix**: measure assembled input size pre-call; shed oldest `function_call` / `function_call_output` pairs to fit a budget (pi's `keepRecentTokens`, ~20K recent). Emit `AgentEvent::ContextTrimmed` as today.
- **Risk**: medium. Must preserve tool_use ↔ tool_result pairing.

#### 4. Budgeted tool-call replay with placeholders
- **Where**: `crates/nav-core/src/context/replay.rs:9` + a new replay-policy module.
- **Problem**: replay skips `ToolCallStarted` / `ToolCallOutput`, so the model loses the tool/result chain and re-reads/re-runs. Conversely, dumping all old outputs back would explode token use.
- **Fix**: budgeted replay (see [Detailed design: replay budget](#detailed-design-replay-budget)).
- **Risk**: medium. Touches event schema and replay.

#### 5. Tighter caps for file reads + bash spillover
- **Where**: `crates/nav-core/src/tool_registry/truncate.rs:16`, `crates/nav-core/src/agent_loop/runner.rs:238`.
- **Problem**: file reads use the generic tool-output cap (2000 lines / 50KB); bash output is kept inline even when huge.
- **Fix**:
  - Adopt Amp's stricter file-mention caps (500 lines / 2KB per line) for `read_file` specifically.
  - For bash outputs >100KB, spill to a temp file, keep only the tail in history, expose `full_output_path` for retrieval.
  - Record `truncated` / `truncated_by` / `full_output_path` metadata on the event.
- **Risk**: low. Pure truncation policy + metadata.

#### 6. Native `read_file` slicing
- **Where**: `tool_definitions`, `run_tool` argument parsing, `fs::read_file`.
- **Problem**: `read_file` only takes `path`, encouraging broad reads. The shell rewrite for `cat` / `head` / `tail` helps only when the model picks shell.
- **Fix**: add `offset` (1-indexed) and `limit` parameters; append next-offset notices on truncation. See [Detailed design: native read slicing](#detailed-design-native-read-slicing).
- **Risk**: low.

#### 7. Artifact-backed full-output retrieval
- **Where**: `ToolCallOutput` event metadata, a new in-session artifact store,
  and a read-only `expand_artifact` / `read_tool_output` tool.
- **Problem**: truncation is currently lossy from the model's point of view. If
  nav keeps less output inline, the model needs a precise way to retrieve the
  full result without repeating broad reads or commands.
- **Fix**: store raw oversized tool output outside normal replay, keep a compact
  summary plus artifact id in context, and expose a retrieval tool that can read
  exact ranges or the full raw artifact. Prefer persisted session artifacts over
  temp files for TUI resume fidelity.
- **Risk**: medium. Must keep artifact reads workspace/session-scoped and avoid
  turning large expansion into the new default.

#### 8. Semantic output reducers
- **Where**: new reducer layer between `tool_registry::run_tool` and
  `function_call_output` assembly.
- **Problem**: generic head/head-tail truncation preserves bytes, not intent. A
  `grep` result, a file read, a bash log, and LSP output each need different
  summaries and follow-up hints.
- **Fix**: add per-tool reducers:
  - `read_file`: outline/imports/signatures/preview for full reads above the
    budget, with artifact id and next slice hint.
  - `code_search`: grouped file summaries and match counts before raw matches.
  - `bash`: status, command, first failure-looking lines, and tail.
  - future web/LSP tools: summaries tuned to their result shapes.
- **Risk**: medium. Summaries must remain deterministic and testable; do not use
  an LLM reducer in the hot path.

#### 9. Context manager for normalized prompt history
- **Where**: new `context/history.rs` or `agent_loop/history.rs`, used by both
  live turns and `rebuild_responses_input`.
- **Problem**: the active loop appends raw Responses JSON, replay skips tools,
  and overflow recovery drops one pair reactively. There is no single owner for
  model-visible history invariants.
- **Fix**: introduce a history manager that:
  - records only API-suitable items,
  - enforces call/output pairing,
  - removes orphan outputs,
  - applies truncation/reducer policy at record time,
  - strips unsupported or old images,
  - reports model-visible bytes/tokens by category.
- **Risk**: high. This is the correct long-term shape, but should follow the
  smaller output and replay changes unless a refactor becomes unavoidable.

#### 10. Historical reasoning and image shedding
- **Where**: replay policy plus active-turn request assembly.
- **Problem**: old reasoning continuations, short assistant narration around
  tool calls, and image payloads can remain expensive long after they stop
  helping the next turn.
- **Fix**:
  - Preserve provider continuation items required for the most recent
    `store: false` tool flow.
  - Replace older reasoning/narration with explicit placeholders once no longer
    needed for continuation.
  - Drop or placeholder images older than a small number of user turns, and
    strip images entirely when the resolved model does not accept image input.
- **Risk**: medium-high. Must not break Responses continuation semantics.

#### 11. Incremental request payloads
- **Where**: `crates/nav-core/src/model/responses/websocket.rs` and request
  state near the transport client.
- **Problem**: with `store: false`, nav sends the full `input` array every turn.
  Even when provider billing still counts the full prompt, this increases
  serialization, transport cost, and makes prompt-cache boundaries harder to
  reason about.
- **Fix**: when the current request is a strict extension of the previous
  request and non-input fields are unchanged, send only the delta with the
  previous response id if the transport/provider supports it. Fall back to the
  current full request path otherwise.
- **Risk**: medium. Treat as an optimization after replay/history correctness.

#### 12. Tool-loop budget backpressure
- **Where**: `run_agent_inner` loop and possibly base instructions.
- **Problem**: a single turn can run many tools before the user sees the result,
  making the next normal turn huge even if total user turns are few.
- **Fix**: lower the default loop cap and inject deterministic budget-check
  messages after N tool calls ("produce a deliverable unless a specific gap
  changes the answer"). Keep an escape hatch for deliberate deep research.
- **Risk**: low-medium. Needs UX tuning so productive coding loops are not cut
  off too early.

### Tier 2 — UX wins (do not move compaction much)

#### 13. Edit / restore previous submitted messages
- **Where**: `crates/nav-tui/src/bottom_pane/slash_popup.rs:14,70`, session event log.
- **Problem**: Amp lets users navigate to an earlier user message, edit it, truncate everything after, and rerun. nav's `/restore` is a git checkpoint restore; `/fork [seq]` creates a new session copy. Neither rewrites the current session.
- **Fix**: event-log truncation at a selected user turn + re-emit. First-class operation, not a fork.
- **Risk**: medium. Persistence semantics + UI.

#### 14. Handoff command
- **Where**: new command alongside `/compact`.
- **Problem**: when the user pivots goals, `/compact` keeps history; a fresh thread loses everything. Amp's handoff uses a second model to extract the relevant messages, tool calls, and files into an editable draft.
- **Fix**: `/handoff <goal>` runs a second-model extraction pass and opens a draft prompt in a fresh session.
- **Risk**: low-medium. Builds on existing session-fork plumbing.

#### 15. Submit-time `@file` scanning
- **Where**: `crates/nav-tui/src/bottom_pane/mention_popup.rs:32`.
- **Problem**: today `@` opens a mention popup that creates a pending attachment. Plain typed `@path` in a message is not resolved on submit (Amp does this).
- **Fix**: scan submitted message text for `@<path>` tokens, resolve + inline under the same truncation policy as #5.
- **Risk**: low.

#### 16. `read_thread` tool
- **Where**: new tool in `crates/nav-core/src/tool_registry/definitions.rs:17`.
- **Problem**: model cannot reference other sessions by ID/URL. Amp does this via `read_thread` + second-model extraction.
- **Fix**: add tool that pulls excerpts from a session by ID with a budget.
- **Risk**: low. Useful but niche until multi-thread workflows are common.

#### 17. Ambient context additions
- **Where**: instruction assembly in `crates/nav-core/src/context/mod.rs:48`.
- **Problem**: Amp injects OS, cwd listing, open file, selected text. nav injects only cwd + skills + project context files.
- **Fix**: add minimal ambient context (open file path, selected text, shallow `ls` of cwd) **gated behind a per-turn size budget**.
- **Risk**: medium. *Adds* per-turn bytes — only ship after Tier 1 lands.

### Execution order

- **Sprint 1 — fix the symptom**: #1 → #2 → #5 → #6 → #3. Each independently shippable.
- **Sprint 2 — make truncation recoverable**: #7 → #8 → #4.
- **Sprint 3 — history correctness and modality pressure**: #9 → #10 → #12.
- **Sprint 4 — transport optimization**: #11.
- **Sprint 5 — UX**: #13 → #14 → #15.
- **Later**: #16, #17.

## Detailed Design: Replay Budget

This is the deep design for **Tier 1 #4**. It is the natural predecessor to any further compaction work; if ordinary-turn replay is wrong or bloated, compaction only hides the symptom later.

### Shape

Today:

```text
session events -> rebuild_responses_input -> user/assistant messages only
```

Target:

```text
session events -> rebuild_responses_input_with_policy -> budgeted Responses input
```

The budgeted input should include:

1. User and assistant messages, as today.
2. Provider continuation items required by Responses `store: false`, especially encrypted reasoning items when present.
3. Function-call items for prior tool calls.
4. Function-call-output items for recent or budget-protected tool outputs.
5. Placeholder outputs for older tool results that are no longer worth replaying raw.

A placeholder should be explicit and stable, e.g.:

```text
[Old tool result content cleared; original output is available in session log]
```

If nav adds artifact-backed full-output retrieval later, the placeholder can include an artifact id. The first slice can rely on the durable session log and focused re-runs.

### Policy rules

Deterministic, easy to test:

- Keep the last `N` user turns' tool outputs verbatim. Start with `N = 2`.
- Keep all tool outputs after the latest user message verbatim while a turn is active.
- Keep failed-tool outputs verbatim for at least the last `N` user turns.
- Keep mutation tool outputs and file-change metadata visible in compact form.
- Replace older successful read/search/bash outputs with placeholders.
- Preserve the matching `function_call` item whenever a `function_call_output` placeholder is emitted, so the transcript stays structurally valid.
- Never replay a `function_call_output` without its matching `function_call`.
- Never replay encrypted reasoning detached from the model output item it is meant to continue.

Initial constants live in one policy module, not scattered through replay:

```rust
pub struct ReplayBudget {
    pub raw_tool_turns: usize,
    pub max_raw_tool_output_bytes: usize,
    pub max_total_tool_output_bytes: usize,
}
```

Suggested defaults (deliberately conservative; tune after `/context all` shows real distributions):

- `raw_tool_turns = 2`
- `max_raw_tool_output_bytes = 50 * 1024`
- `max_total_tool_output_bytes = 120 * 1024`

### Provider continuation items

nav already requests `reasoning.encrypted_content` from the Responses API. With `store: false`, that encrypted reasoning is the provider-safe way to continue tool-call conversations without server-side state.

- Decode `response.output_item.done` items of type `reasoning`.
- Persist a durable event for replayable provider output items, or add a dedicated `AgentEvent::Reasoning` if the narrower shape is sufficient.
- Do not persist plaintext hidden reasoning. Store only provider-returned encrypted continuation content and any public summary fields the API needs.
- Keep existing user-facing `AssistantMessageDone` behavior for visible assistant text.

The replay layer can then reconstruct the Responses `input` array without relying on the in-memory `raw_output` vector that exists only inside one `run_agent` call.

## Detailed Design: Native Read Slicing

Deep design for **Tier 1 #6**. nav's `read_file` currently accepts only `path`, encouraging broad reads on large files. Shell rewriting for `cat` / `head` / `tail` only helps when the model picks shell.

Extend `read_file`:

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
- When output is truncated by default line/byte caps, append the next offset.
- When a user-provided `limit` stops before EOF, append a remaining-lines notice.
- Preserve existing path-security, protected-read, and skill-dir behavior.

Combined with the tighter caps in Tier 1 #5, this gives the model a safe way to inspect a file slice rather than pulling the whole file or falling back to shell.

## User Stories

1. The agent remembers which files it already read last turn, so it doesn't re-read them on a new prompt.
2. Recent tool outputs remain visible to the model, so immediate debugging context is not lost.
3. Old bulky tool outputs stop inflating every normal turn, so I have more useful work before compaction.
4. Stale tool outputs become clear placeholders, so the model knows a tool ran even when old content is cleared.
5. Failed commands and recent errors are preserved longer than successful reads.
6. `read_file` supports `offset` and `limit` for targeted slices.
7. Truncation messages tell the model how to continue, so it can choose the next narrow read.
8. `/context all` explains which tool outputs are raw vs cleared, so token growth is inspectable.
9. NDJSON event log remains truthful even when model replay is pruned.
10. Replay budgeting is testable independently from live model calls.
11. The system prompt does not balloon as the skills catalog grows (Tier 1 #1).
12. Oversized output is recoverable by artifact id instead of by re-running the
    broad command/read.
13. Old images and old reasoning/narration stop inflating later turns once they
    are no longer needed for continuation.
14. I can edit an earlier user message and rerun from that point (Tier 2 #13).

## Implementation Plan

### Sprint 1 — fix the symptom

1. **Instrument first**. Log per-turn input bytes broken down by `instructions / tools / history / tool_outputs`. Surface in `/context` (`crates/nav-core/src/context/report.rs`). Without this, the rest is guesswork.
2. **Tier 1 #1 — lazy skills section**. Edit `skill_instruction_section` to emit name + 1-line summary; update wrapper text to tell the model how to load SKILL.md.
3. **Tier 1 #2 — caching**. Mark the instructions block and tool-definitions block as cacheable in the Responses request.
4. **Tier 1 #5 — caps + bash spillover**. Update `truncate.rs` to specialize `read_file` caps; add disk spillover for large bash output with `full_output_path` metadata on the event.
5. **Tier 1 #6 — native `read_file` slicing**. Add `offset`/`limit` to the tool, parse args, propagate to `fs::read_file_slice`. Update truncation notices.
6. **Tier 1 #3 — proactive pruning**. Add a pre-call size check in the agent loop; shed oldest tool pairs to fit budget. Snapshot test the pruning decisions.

### Sprint 2 — make truncation recoverable

7. **Tier 1 #7 — artifact-backed full-output retrieval**. Persist raw oversized outputs outside normal replay, emit artifact ids, and add a read-only expansion tool.
8. **Tier 1 #8 — semantic output reducers**. Add deterministic reducers for `read_file`, `code_search`, and `bash` before old outputs enter history.
9. **Tier 1 #4 — replay policy module**. Create `crates/nav-core/src/agent_loop/replay_policy.rs` (or near replay). Define `ReplayBudget`. Move policy decisions out of `replay.rs`.
10. **Persist replayable provider output**. Extend `ResponseItem` for reasoning items; capture them from `response.output_item.done`; persist as a durable event before tool execution emits `ToolCallStarted` / `ToolCallOutput`.
11. **Replay tool calls under budget**. Update `rebuild_responses_input` to emit `function_call` + (raw, reduced, or placeholder) `function_call_output`, plus provider continuation items. Retain the aborted-turn truncation behavior.
12. **Extend `/context` reporting**. Label entries `tool output 3 (raw)`, `tool output 4 (reduced)`, `tool output 5 (cleared)`, `reasoning continuation`. PR auditable without live API calls.

### Sprint 3 — history correctness and modality pressure

13. **Tier 1 #9 — normalized prompt history manager**. Centralize model-visible history recording, pairing, modality stripping, and token-policy truncation.
14. **Tier 1 #10 — historical reasoning and image shedding**. Strip older reasoning/narration and old image payloads without breaking recent `store: false` continuation.
15. **Tier 1 #12 — tool-loop budget backpressure**. Lower/default caps and inject budget-check messages after repeated tool calls.

### Sprint 4 — transport optimization

16. **Tier 1 #11 — incremental request payloads**. Send websocket deltas for strict extensions when supported; retain full-request fallback.

### Sprint 5 — UX

17. **Tier 2 #13** — edit/restore previous submitted messages (event-log truncation + re-emit).
18. **Tier 2 #14** — `/handoff <goal>`.
19. **Tier 2 #15** — submit-time `@file` scanning that reuses #5's truncation policy.

### Later

20. **Tier 2 #16** — `read_thread` tool.
21. **Tier 2 #17** — ambient context additions, gated by size budget.

## Testing Decisions

Focused tests before broad integration:

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
- `skill_instruction_section_emits_only_name_and_summary`
- `agent_loop_proactively_drops_oldest_pair_at_budget`
- `bash_output_above_threshold_spills_to_disk_with_pointer`
- `artifact_store_round_trips_truncated_tool_output`
- `read_file_full_read_reduces_to_outline_with_artifact_id`
- `code_search_reducer_groups_matches_by_file`
- `history_manager_removes_orphan_tool_outputs`
- `history_manager_strips_images_when_model_lacks_image_input`
- `replay_strips_old_reasoning_but_keeps_recent_continuation`
- `agent_loop_injects_budget_check_after_repeated_tool_calls`
- `websocket_request_uses_incremental_delta_for_strict_extension`

Snapshot tests (`insta`) for system-prompt assembly so #1 and #2 do not silently change wording.

Run at minimum:

```text
cargo test -p nav-core replay
cargo test -p nav-core context_report
cargo test -p nav-core tool_registry::
cargo test -p nav-tui
```

Before landing, the broader gate:

```text
cargo test -p nav-core -p nav-tui
cargo clippy --workspace --all-targets -- -D warnings
```

## Acceptance Criteria

- A normal TUI turn after a tool-using turn replays the prior tool call and a budgeted result or placeholder.
- Recent successful tool outputs remain raw; older successful bulky outputs become placeholders.
- Failed recent tool outputs remain raw.
- Responses continuation items needed for `store: false` replay are persisted durably.
- `/context all` shows raw vs cleared tool-output entries and per-block byte counts (instructions / tools / history / tool_outputs).
- `read_file` supports `offset` and `limit` with next-offset truncation notices.
- The system prompt skills block emits only name + 1-line summary; SKILL.md is loaded on demand.
- Instructions + tool definitions blocks hit the provider prompt cache after turn 1.
- Pre-call pruning drops oldest tool pairs to a budget instead of waiting for `ContextWindowExceeded`.
- Bash outputs above the spillover threshold are stored to disk with a `full_output_path` pointer on the event.
- Oversized read/search/bash outputs have a recoverable artifact id, and the
  expansion tool can retrieve the exact raw result without re-running the
  original tool.
- Reducers produce deterministic compact output for full-file reads, code
  search, and bash logs.
- Model-visible history is normalized: no orphan tool outputs, no outputs
  without calls, no unsupported image payloads.
- Old images and old reasoning/narration are stripped or replaced without
  dropping recent provider continuation items needed by `store: false`.
- Tool-loop budget checks fire before a runaway tool-heavy turn consumes the
  next turn's context budget.
- Incremental websocket requests are used only for strict request extensions and
  safely fall back to full requests.
- Existing compacted-session replay still starts from the latest checkpoint, but post-checkpoint normal turns use the same pre-compaction replay policy.
- No compaction prompt, summary format, or threshold behavior changes.

## Open Questions

- Should replay budgeting be configurable in `.nav/settings.json` immediately, or kept internal until defaults are proven?
- Should placeholders include a session event sequence number so a future `expand_event_output` tool can recover the exact old output?
- Should mutation outputs be replayed raw, or should `FileChange` summaries become the model-visible source of truth for mutations?
- Should `read_file` default to a smaller line window than the global 50KB / 2000-line cap once `offset`/`limit` exists?
- Should the lazy skills block include a one-line "how to discover the full list" hint, or rely on a static section in base instructions?
- Should raw tool artifacts be persisted in SQLite, sidecar files under `.nav`,
  or ephemeral memory with only same-process expansion?
- Should artifact expansion support ranges from day one, or should range support
  wait until reducer output proves the need?
- What is the right default keep window for historical reasoning and old images:
  last assistant message, last user turn, or last two user turns?
- Should incremental request deltas be websocket-only, or should the SSE path
  learn the same previous-response-id optimization if the provider supports it?

## Sources

- Amp Context Management guide — https://ampcode.com/guides/context-management
- Sibling reference repos (read-only): `../pi`, `../opencode`, `../codex`, `../kimiflare`.
- Related: [long-session-compaction-prd.md](long-session-compaction-prd.md) (the *post*-compaction work this plan deliberately does not touch).
