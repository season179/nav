# Context Management Plan

Combines two investigations and one prior PRD into a single ranked roadmap:

- **pi comparison** — what `../pi` does turn-by-turn that nav lacks.
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
- Completed tool parts can be marked compacted.
- Replay renders compacted old outputs as `[Old tool result content cleared]`.
- Recent/protected context kept; stale large outputs cleared before they inflate future turns.

### kimiflare
- Reduced outputs include an artifact id.
- `expand_artifact` retrieves the full raw output when needed.
- read/grep/bash/web fetch tools are designed around reduced summaries plus targeted expansion.

### Amp
- Submit-time `@<filename>` scanning resolves and inlines files at message send.
- File-mention truncation policy: 500 lines / 2KB per line.
- Ambient context: OS, cwd listing, open file, selected text.
- Edit / restore-to-earlier-message rewrites the current thread.
- Handoff: second-model extraction of relevant messages, tool calls, and files into a draft prompt for a fresh thread.
- `read_thread` tool to reference other threads by ID/URL.

The useful lessons:

- **From pi**: ordinary tool use should lead the model toward smaller follow-up reads; per-event exclusion is cheaper than re-summarizing.
- **From opencode**: old tool results should become explicit placeholders, not disappear and not stay raw forever.
- **From kimiflare**: the model can act on a compact view if the full output remains recoverable through an explicit path.
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

### Tier 2 — UX wins (do not move compaction much)

#### 7. Edit / restore previous submitted messages
- **Where**: `crates/nav-tui/src/bottom_pane/slash_popup.rs:14,70`, session event log.
- **Problem**: Amp lets users navigate to an earlier user message, edit it, truncate everything after, and rerun. nav's `/restore` is a git checkpoint restore; `/fork [seq]` creates a new session copy. Neither rewrites the current session.
- **Fix**: event-log truncation at a selected user turn + re-emit. First-class operation, not a fork.
- **Risk**: medium. Persistence semantics + UI.

#### 8. Handoff command
- **Where**: new command alongside `/compact`.
- **Problem**: when the user pivots goals, `/compact` keeps history; a fresh thread loses everything. Amp's handoff uses a second model to extract the relevant messages, tool calls, and files into an editable draft.
- **Fix**: `/handoff <goal>` runs a second-model extraction pass and opens a draft prompt in a fresh session.
- **Risk**: low-medium. Builds on existing session-fork plumbing.

#### 9. Submit-time `@file` scanning
- **Where**: `crates/nav-tui/src/bottom_pane/mention_popup.rs:32`.
- **Problem**: today `@` opens a mention popup that creates a pending attachment. Plain typed `@path` in a message is not resolved on submit (Amp does this).
- **Fix**: scan submitted message text for `@<path>` tokens, resolve + inline under the same truncation policy as #5.
- **Risk**: low.

#### 10. `read_thread` tool
- **Where**: new tool in `crates/nav-core/src/tool_registry/definitions.rs:17`.
- **Problem**: model cannot reference other sessions by ID/URL. Amp does this via `read_thread` + second-model extraction.
- **Fix**: add tool that pulls excerpts from a session by ID with a budget.
- **Risk**: low. Useful but niche until multi-thread workflows are common.

#### 11. Ambient context additions
- **Where**: instruction assembly in `crates/nav-core/src/context/mod.rs:48`.
- **Problem**: Amp injects OS, cwd listing, open file, selected text. nav injects only cwd + skills + project context files.
- **Fix**: add minimal ambient context (open file path, selected text, shallow `ls` of cwd) **gated behind a per-turn size budget**.
- **Risk**: medium. *Adds* per-turn bytes — only ship after Tier 1 lands.

### Execution order

- **Sprint 1 — fix the symptom**: #1 → #2 → #3 → #5. Each independently shippable. #1 alone may resolve the 2-turn issue.
- **Sprint 2 — durability**: #4 → #6.
- **Sprint 3 — UX**: #7 → #8 → #9.
- **Later**: #10, #11.

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
12. I can edit an earlier user message and rerun from that point (Tier 2 #7).

## Implementation Plan

### Sprint 1 — fix the symptom

1. **Instrument first**. Log per-turn input bytes broken down by `instructions / tools / history / tool_outputs`. Surface in `/context` (`crates/nav-core/src/context/report.rs`). Without this, the rest is guesswork.
2. **Tier 1 #1 — lazy skills section**. Edit `skill_instruction_section` to emit name + 1-line summary; update wrapper text to tell the model how to load SKILL.md.
3. **Tier 1 #2 — caching**. Mark the instructions block and tool-definitions block as cacheable in the Responses request.
4. **Tier 1 #5 — caps + bash spillover**. Update `truncate.rs` to specialize `read_file` caps; add disk spillover for large bash output with `full_output_path` metadata on the event.
5. **Tier 1 #3 — proactive pruning**. Add a pre-call size check in the agent loop; shed oldest tool pairs to fit budget. Snapshot test the pruning decisions.

### Sprint 2 — durability

6. **Tier 1 #6 — native `read_file` slicing**. Add `offset`/`limit` to the tool, parse args, propagate to `fs::read_file_slice`. Update truncation notices.
7. **Tier 1 #4 — replay policy module**. Create `crates/nav-core/src/agent_loop/replay_policy.rs` (or near replay). Define `ReplayBudget`. Move policy decisions out of `replay.rs`.
8. **Persist replayable provider output**. Extend `ResponseItem` for reasoning items; capture them from `response.output_item.done`; persist as a durable event before tool execution emits `ToolCallStarted` / `ToolCallOutput`.
9. **Replay tool calls under budget**. Update `rebuild_responses_input` to emit `function_call` + (raw or placeholder) `function_call_output`, plus provider continuation items. Retain the aborted-turn truncation behavior.
10. **Extend `/context` reporting**. Label entries `tool output 3 (raw)`, `tool output 4 (cleared)`, `reasoning continuation`. PR auditable without live API calls.

### Sprint 3 — UX

11. **Tier 2 #7** — edit/restore previous submitted messages (event-log truncation + re-emit).
12. **Tier 2 #8** — `/handoff <goal>`.
13. **Tier 2 #9** — submit-time `@file` scanning that reuses #5's truncation policy.

### Later

14. **Tier 2 #10** — `read_thread` tool.
15. **Tier 2 #11** — ambient context additions, gated by size budget.

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
- Existing compacted-session replay still starts from the latest checkpoint, but post-checkpoint normal turns use the same pre-compaction replay policy.
- No compaction prompt, summary format, or threshold behavior changes.

## Open Questions

- Should replay budgeting be configurable in `.nav/settings.json` immediately, or kept internal until defaults are proven?
- Should placeholders include a session event sequence number so a future `expand_event_output` tool can recover the exact old output?
- Should mutation outputs be replayed raw, or should `FileChange` summaries become the model-visible source of truth for mutations?
- Should `read_file` default to a smaller line window than the global 50KB / 2000-line cap once `offset`/`limit` exists?
- Should the lazy skills block include a one-line "how to discover the full list" hint, or rely on a static section in base instructions?

## Sources

- Amp Context Management guide — https://ampcode.com/guides/context-management
- Sibling reference repos (read-only): `../pi`, `../opencode`, `../kimiflare`.
- Related: [long-session-compaction-prd.md](long-session-compaction-prd.md) (the *post*-compaction work this plan deliberately does not touch).
