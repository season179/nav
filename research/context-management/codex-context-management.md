# Codex Context Management — Architecture Report

> Scope: `/Users/season/Personal/codex` as of HEAD on 2026-05-27. All citations are `path:line` relative to that root unless noted; only files outside that path are flagged.

## 1. Executive Summary

Codex's context management is centered on a single Rust agent loop in `codex-rs/core` that streams against OpenAI's Responses API (HTTP or WebSocket). The loop maintains an in-memory `ContextManager` of `ResponseItem`s, persists the same items to a JSONL **rollout file** on disk, and re-derives the full input array on every sampling request — there is no client-side prompt sharding or cache-breakpoint engineering beyond a per-thread `prompt_cache_key` that lets the server reuse its own KV cache.

Token accounting is heuristic (a flat 4 bytes ≈ 1 token; `codex-rs/utils/string/src/truncate.rs:4`) and refined by server-reported `TokenUsage` whenever a response completes. When token usage crosses the configured `model_auto_compact_token_limit` (or the full context window), the same loop kicks off **auto-compaction**, either *local* (the model is asked to write a Memento-style handoff summary that replaces history) or *remote* (the model emits a `Compaction { encrypted_content }` item via `/responses` with a `CompactionTrigger`, and the v2 path retains recent user/developer messages truncated to a 64k-token budget).

Cross-session memory is a separate, file-backed pipeline rooted at `$CODEX_HOME/memories/` (`codex-rs/memories/`): a background Phase 1 extracts per-rollout memories into the state DB and Phase 2 consolidates them into a git-tracked workspace including `MEMORY.md`, `raw_memories.md`, and `rollout_summaries/` — written by a dedicated sub-agent. Subagents themselves are first-class: `AgentControl::spawn_agent` creates a fresh `Codex` thread or forks a parent's rollout (`FullHistory` or `LastNTurns`); inter-agent results flow back via assistant-role `InterAgentCommunication` envelopes that the parent's history treats as turn boundaries.

Non-obvious choices: heuristic tokenizer (no tiktoken), per-thread rather than per-turn prompt-cache identity, a 4× truncation budget on tool outputs at record time, a two-scope budget design (`Total` vs `BodyAfterPrefix`), and the "previous-model downshift" pre-turn compaction. Risky areas: byte→token estimator drifts from real tokenizers; reasoning items use a hand-tuned formula; `codex-core/session/mod.rs` is 3,339 LoC; the only "context shrink" mechanism between full and compact is dropping the oldest item on a `ContextWindowExceeded` retry (`compact.rs:230`).

## 2. End-to-End Turn Trace

The orchestration sequence for one "regular" user turn:

1. **Submission lands as an `Op::UserInput`** and reaches `user_input_or_turn_inner` (`codex-rs/core/src/session/handlers.rs:186`). That creates a new `TurnContext`, queues the items, and calls `sess.spawn_task(..., RegularTask::new())` (`handlers.rs:249-254`).

2. **`RegularTask::run` emits `TurnStarted` and calls `run_turn`** (`codex-rs/core/src/tasks/regular.rs:73-83`). The task wrapper re-enters `run_turn` if pending input accumulated during the run (`regular.rs:84-88`).

3. **`run_turn`** (`codex-rs/core/src/session/turn.rs:132`) does, in order:
   - **Pre-sampling compaction**: `run_pre_sampling_compact` (`turn.rs:146,693`). First attempts a model-downshift compact (`maybe_run_previous_model_inline_compact`, `turn.rs:719`); then, if `auto_compact_token_status` reports `token_limit_reached`, runs `run_auto_compact(..., InitialContextInjection::DoNotInject, ContextLimit, PreTurn)` (`turn.rs:701-710`).
   - **Record context updates & set reference** (`turn.rs:160`): `record_context_updates_and_set_reference_context_item` (`session/mod.rs:2929`) either injects full initial context (when no baseline exists) or only diff items vs the last `reference_context_item`.
   - **Skills/plugin injection** (`turn.rs:163-165 build_skills_and_plugins`).
   - **Run session-start hooks** (`turn.rs:166`).
   - **Record incoming user input** (`turn.rs:170 run_hooks_and_record_inputs`).
   - **Inner sampling loop** (`turn.rs:215`): repeatedly drains pending input, clones history (`sess.clone_history().await.for_prompt(...)`, `turn.rs:230-234`), and calls `run_sampling_request`.

4. **`run_sampling_request`** (`turn.rs:893`):
   - Builds a tool router via `built_tools` (`turn.rs:903,991`) which merges MCP server tools, plugin connectors, discoverable apps, dynamic tools, and extension executors into a `ToolRouter`.
   - Reads `base_instructions` from session state (`turn.rs:905` → `session/mod.rs:1135`).
   - Calls `build_prompt` (`turn.rs:864-881`) to assemble a `Prompt { input, tools, parallel_tool_calls, base_instructions, personality, output_schema, output_schema_strict }`.
   - Calls `try_run_sampling_request` (`turn.rs:940,1651`) which delegates to `ModelClientSession::stream`.

5. **Request assembly** in `ModelClient::build_responses_request` (`codex-rs/core/src/client.rs:717`):
   - `instructions = prompt.base_instructions.text.clone()` (`client.rs:726,755`).
   - `input = prompt.get_formatted_input()` (raw `ResponseItem` array, `client.rs:727`).
   - `tools = create_tools_json_for_responses_api(&prompt.tools)?` (`client.rs:728`).
   - `prompt_cache_key = Some(self.state.thread_id.to_string())` (`client.rs:751`).
   - `include = ["reasoning.encrypted_content"]` when reasoning is on (`client.rs:730-734`).
   - `parallel_tool_calls`, `service_tier`, `text` (verbosity + JSON schema), and identity headers are added.

6. **`stream`** (`client.rs:1570`) picks WebSocket or HTTP transport and returns a `ResponseStream`. Inside `try_run_sampling_request`, the stream's events are consumed: each `OutputItemDone` is funneled to `handle_output_item_done` (which dispatches function calls through `ToolRouter` and appends results to history), `Completed` updates `TokenUsage` via `update_token_usage_info` (`session/mod.rs:2961`), and reasoning encrypted content is preserved verbatim on `Reasoning` items.

7. **After sampling**, back in `run_turn`'s loop body (`turn.rs:249-356`):
   - `auto_compact_token_status` is recomputed (`turn.rs:258`).
   - If `token_limit_reached && needs_follow_up`, **mid-turn compaction** runs with `InitialContextInjection::BeforeLastUserMessage` (`turn.rs:283-309`).
   - If the model returned no more pending tool calls and no follow-up input, stop hooks fire (`turn.rs:313`) and the loop exits.
   - Errors like `ContextWindowExceeded` cause `sess.set_total_tokens_full(...)` and surface (`turn.rs:956-959`).

8. **The same loop runs repeatedly until** the model produces an assistant message with no follow-up tools and stop-hooks let it terminate (`turn.rs:341-355`).

## 3. Subsystem Findings

### 3.1 Per-turn request assembly

- **System prompt** — Codex sends a single `instructions` field, not a system message. Resolution order (`session/mod.rs:541-545`):
  1. `config.base_instructions` override
  2. The resumed session's recorded `base_instructions`
  3. `model_info.get_model_instructions(personality)` (model-family default + personality)

  It is stored on `SessionConfiguration` and read via `Session::get_base_instructions` (`session/mod.rs:1135`).

- **Tool defs** — Built per request inside `built_tools` (`turn.rs:991`); the router fans in MCP tools, app connectors, plugin tools, discoverable suggestions, dynamic tools (from spawn args / persisted state), and built-ins, and exposes them as `model_visible_specs()` (`turn.rs:872`). JSON encoding is done in `create_tools_json_for_responses_api` (`client.rs:728`).

- **Message history** — The full conversation lives in `ContextManager` (`codex-rs/core/src/context_manager/history.rs:34`). Each turn re-derives the input array by cloning history and calling `for_prompt(input_modalities)` (`history.rs:119-122`), which:
  - Ensures every call has its output (`history.rs:368 normalize::ensure_call_outputs_present`).
  - Removes orphan outputs (`history.rs:371`).
  - Strips images when the model doesn't support them (`history.rs:374`).

- **Injected context** — `Session::build_initial_context` (`session/mod.rs:2670`) aggregates *developer-role* sections (model switch notice, permissions instructions, developer instructions, collaboration-mode instructions, realtime updates, personality spec, app/connector instructions, available skills, available plugins, extension contributors) plus *contextual-user* sections (user instructions, env context with shell/subagents) into one or more bundled `ResponseItem::Message`s (`session/mod.rs:2857-2895`). On subsequent turns with an intact `reference_context_item`, only diff items are emitted via `build_settings_update_items` (`session/mod.rs:2942`).

- **Prompt-cache breakpoints** — Codex does **not** insert anthropic-style cache markers. It uses the OpenAI Responses API's `prompt_cache_key` set to `thread_id` (`client.rs:751`). The `window_generation` counter (`client.rs:381 advance_window_generation`) is bumped on history rewrites so that requests in the new window do not reuse server-side incremental request state but still share the cache key.

### 3.2 Compaction

**Triggers** — `auto_compact_token_status` (`turn.rs:641-691`) computes `token_limit_reached` two ways depending on `model_auto_compact_token_limit_scope`:
- `Total`: `active_context_tokens ≥ auto_compact_token_limit` (`turn.rs:650-657`)
- `BodyAfterPrefix`: `(active_context_tokens − window.prefill_input_tokens) ≥ limit` (`turn.rs:658-672`), plus `full_context_window_limit_reached` independently (`turn.rs:674-677`).

Compaction is invoked in three places: pre-turn (`turn.rs:701`), mid-turn (`turn.rs:283`), and on previous-model downshift (`turn.rs:758`). Manual `/compact` flows through `run_compact_task` (`compact.rs:96` / `compact_remote.rs:59` / `compact_remote_v2.rs:74`).

**Algorithm A — Local (`codex-rs/core/src/compact.rs`)**:
1. Append the synthesized prompt (template `core/templates/compact/prompt.md`) as a user message to a *cloned* history (`compact.rs:182-186`).
2. Stream a model turn against that history; on `ContextWindowExceeded`, drop the oldest item (`compact.rs:224-232`) and retry.
3. After completion, take the last assistant message as the summary suffix; prepend `SUMMARY_PREFIX` (`compact.rs:262-263`).
4. Rebuild new history with `build_compacted_history` (`compact.rs:466-530`): the last `COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000` tokens' worth of pre-existing user messages, then one synthetic user message containing the summary.
5. If `InitialContextInjection::BeforeLastUserMessage`, splice fresh initial-context items just before the last real user (`compact.rs:267-275`, helper `insert_initial_context_before_last_real_user_or_summary:419`).
6. `replace_compacted_history` (`session/mod.rs:2609`) atomically swaps the in-memory history, advances the auto-compact window, and persists a `RolloutItem::Compacted` to the JSONL.

The local compaction prompt is short and explicit (`core/templates/compact/prompt.md`):

```
You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff
summary for another LLM that will resume the task.

Include:
- Current progress and key decisions made
- Important context, constraints, or user preferences
- What remains to be done (clear next steps)
- Any critical data, examples, or references needed to continue
```

**Algorithm B — Remote v1 (`compact_remote.rs`)** uses the dedicated `/responses/compact` endpoint via `compact_conversation_history` (`client.rs:437`); it filters returned items with `should_keep_compacted_history_item` (`compact_remote.rs:290`): drops developer and non-message user wrappers, keeps assistant, summary, and compaction items.

**Algorithm C — Remote v2 (`compact_remote_v2.rs`)**:
1. `trim_function_call_history_to_fit_context_window` removes trailing codex-generated items until estimated tokens fit (`compact_remote.rs:358-385`).
2. Append a `ResponseItem::CompactionTrigger` sentinel and stream `/responses` (`compact_remote_v2.rs:199-209`).
3. Collect the first `ResponseItem::Compaction { encrypted_content }` and a response_id (`compact_remote_v2.rs:352-396`).
4. `build_v2_compacted_history` retains only user/developer/system messages (`compact_remote_v2.rs:413-419`), then `truncate_retained_messages_for_remote_compaction` keeps the *most recent* messages up to `RETAINED_MESSAGE_TOKEN_BUDGET = 64_000` tokens (`compact_remote_v2.rs:421-445`), then appends the compaction item last.

**Preserved**: the most-recent user messages (always), one synthetic summary or encrypted compaction blob, and on mid-turn compaction the freshly rebuilt initial context block. **Dropped**: assistant messages, reasoning blocks, function calls and outputs, tool search calls, web search calls, prior developer items (since they'll be reinjected when needed).

**Where the summary lands**: the synthetic summary message is inserted *last* in the new history (local path, `compact.rs:522-527`); the encrypted compaction item is appended last (remote v2, `compact_remote_v2.rs:409`). For mid-turn compaction, initial context is spliced *above the last real user message or summary* (`compact.rs:419-464`).

### 3.3 Memory

**In-session state** lives on `SessionState` (`codex-rs/core/src/state/session.rs:22`):
- `history: ContextManager` (the conversation transcript).
- `token_info: Option<TokenUsageInfo>` (server-reported + estimated; `state/session.rs:106`).
- `auto_compact_window: AutoCompactWindow` (ordinal + prefill baseline; `state/auto_compact_window.rs:16-24`).
- `previous_turn_settings` (model/realtime tracking for downshift detection; `state/session.rs:71`).
- `active_connector_selection`, `granted_permissions`, `mcp_dependency_prompted`, `next_turn_is_first` — all session-scoped and dropped on process exit.

**Cross-session persistence** has three layers:

1. **Rollout files (per session)** — `RolloutRecorder` (`codex-rs/rollout/src/recorder.rs:73`) writes every `RolloutItem` (user prompts, assistant items, reasoning, tool calls/outputs, context updates, compaction items, events) as JSONL to `~/.codex/sessions/rollout-<date>-<conversation_id>.jsonl` (`rollout/src/recorder.rs:1348`). Resume reads this file and replays into `ContextManager` (`session/mod.rs:1184-1189`).

2. **State DB (SQLite)** — schema at `codex-rs/state/migrations/` with tables for memories (`0006_memories.sql`), memory usage (`0016_memory_usage.sql`), threads, and thread-spawn edges. Threads, dynamic tools, agent nicknames/roles persist here.

3. **Memory pipeline** (`codex-rs/memories/`, documented in `memories/README.md:31-152`):
   - **Phase 1** runs on root session startup (skipped for ephemeral or sub-agent sessions). For each eligible recent rollout, the model emits structured output (`raw_memory`, `rollout_summary`, `rollout_slug`). Secrets are redacted; results land in the state DB.
   - **Phase 2** holds a global lock; loads the top-N stage-1 outputs ranked by `usage_count` then recency; syncs files under `$CODEX_HOME/memories/`:
     - `raw_memories.md` (deterministic ordering by thread-id)
     - `rollout_summaries/<id>.md`
     - `phase2_workspace_diff.md` (git-style diff against last successful baseline)
   - When the workspace is dirty, an **internal consolidation sub-agent** is spawned (no approvals, no network, local writes only, `Feature::Collab` disabled — see `memories/README.md:111-117`) to update `MEMORY.md`, `memory_summary.md`, and `skills/`. The memories root is a git baseline at `$CODEX_HOME/memories/.git`.
   - The read path lives in `codex-rs/memories/read/src/lib.rs:13 memory_root`. `~/.codex/memories` is auto-added to writable roots in `workspace-write` sandbox mode (`codex-rs/README.md:88`) so the read template can cite it.

The memory subsystem is **not** a transcript shim — memories are referenced as filesystem artifacts and used through citations (`codex-rs/memories/read/src/citations.rs`), not silently pasted into prompts.

### 3.4 Token budgeting

- **Tokenizer**: heuristic, *not* tiktoken. `const APPROX_BYTES_PER_TOKEN: usize = 4` (`codex-rs/utils/string/src/truncate.rs:4`); `approx_token_count`, `approx_bytes_for_tokens`, `approx_tokens_from_byte_count` are all simple integer math (`truncate.rs:71-84`):

```rust
pub fn approx_token_count(text: &str) -> usize {
    let len = text.len();
    len.saturating_add(APPROX_BYTES_PER_TOKEN.saturating_sub(1)) / APPROX_BYTES_PER_TOKEN
}
```

  The authoritative count comes from server `TokenUsage` events (`session/mod.rs:2976-2988`).

- **Per-turn split**: There is no formal "budget split" between system/tools/history. The model receives `{ instructions, input, tools }`; whatever fits in the context window is sent. The only proactive controls are:
  - `model_auto_compact_token_limit` (configurable per scope) — compaction trigger budget.
  - `model_context_window` — full window limit per model.
  - `RETAINED_MESSAGE_TOKEN_BUDGET = 64_000` for remote-v2 compaction retention.
  - `COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000` for local-compaction user-message retention.
  - `default_skill_metadata_budget(context_window)` — skill metadata sizing inside `build_initial_context`.

- **Per-item truncation**: at *record* time, every `FunctionCallOutput`/`CustomToolCallOutput` is truncated by `truncate_function_output_payload` with a `1.2×` serialization budget (`history.rs:377-388`). Truncation policy lives on `TurnContext::truncation_policy` (`session/turn.rs:2546 record_items`).

- **Backpressure**:
  - Mid-turn `ContextWindowExceeded` (`turn.rs:956-959`) → set tokens to full and propagate.
  - During compaction, oldest items are dropped one-by-one on `ContextWindowExceeded` (`compact.rs:224-232`).
  - During remote-v2 compaction, the prep step `trim_function_call_history_to_fit_context_window` removes trailing codex-generated items until the estimate fits (`compact_remote.rs:358-385`).
  - Invalid images in tool outputs get replaced rather than failing (`turn.rs:362-380`, `history.rs:194-224 replace_last_turn_images`).
  - No tail truncation or sliding-window dropping in normal operation — compaction is the only structural shrink.

- **Token estimation per item** (`history.rs:516-573 estimate_response_item_model_visible_bytes`): serialize to JSON, get byte length; **subtract** raw base64 image payload bytes and **add** a per-image estimate (7,373 bytes default for resized; patch-based for `detail: "original"`, capped at 10,000 patches; `history.rs:525-545`). Encrypted reasoning content uses `len*3/4 - 650` (`history.rs:504-509`). Encrypted function outputs use `len*9/16` ceiling (`history.rs:512-514`).

### 3.5 Subagents

Yes, codex has subagents. Two integration paths and a third interactive flow:

1. **`AgentControl::spawn_agent_internal`** (`codex-rs/core/src/agent/control.rs:213`) creates a fresh thread or forks the parent's rollout.

2. **Fork modes** (`agent/control.rs:48-51`):
   - `FullHistory`: copy all rollout items; preserves `reference_context_item` (`control.rs:447`).
   - `LastNTurns(n)`: `truncate_rollout_to_last_n_fork_turns` cuts to the last n user-turn boundaries (`control.rs:412-414`, helper in `thread_rollout_truncation.rs:57-101`).

3. **`run_codex_thread_interactive`** in `codex_delegate.rs:65` — interactive sub-Codex. Inherits from the parent: `installation_id`, `auth_manager`, `models_manager`, `environment_manager`, `skills_manager`, `plugins_manager`, `mcp_manager`, `extensions`, `exec_policy`, `agent_control`, `attestation_provider`, `thread_store`, environment_selections (`codex_delegate.rs:79-102`). It marks `session_source: SessionSource::SubAgent(SubAgentSource::*)` and `thread_source: Some(ThreadSource::Subagent)` so memory generation and other root-only behavior are gated off.

**Isolation**:
- A fresh subagent has its *own* `Session`, `ContextManager`, `ModelClient` (with a different `thread_id`, so its `prompt_cache_key` is independent — `client.rs:751`).
- Subagent rollouts use `persist_extended_history: false` (`codex_delegate.rs:92`) so they don't get full event recording.
- Memory pipeline is skipped (`memories/README.md:35`).
- Subagents inherit shell snapshot, exec policy, and connector state but not in-memory turn state.

**Inheritance**:
- Forks carry full prior history and reference context, modulo `LastNTurns` truncation and filtering of `MultiAgentV2` usage-hint messages (`control.rs:447-470`).
- A `subagent_usage_hint_text` developer message is added on forks under `MultiAgentV2` (`control.rs:471-481`).

**Reintegration**:
- Parent and child exchange via `Op::InterAgentCommunication { communication }` (`protocol.rs:726`).
- `InterAgentCommunication` (`codex-rs/protocol/src/protocol.rs:663-712`) is serialized JSON pushed as an **assistant** message with `phase: Commentary` into the recipient's history:

```rust
pub fn to_response_input_item(&self) -> ResponseInputItem {
    ResponseInputItem::Message {
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: serde_json::to_string(self).unwrap_or_default(),
        }],
        phase: Some(MessagePhase::Commentary),
    }
}
```

- These messages count as turn boundaries in history (`context_manager/history.rs:746-757 is_user_turn_boundary` checks `InterAgentCommunication::is_message_content`), so they participate in rollback and compaction logic.
- A completion watcher (`agent/control.rs:345-351 maybe_start_completion_watcher`) tracks non-MultiAgentV2 agents until terminal status.

## 4. Non-obvious Design Choices

- **Heuristic 4-bytes-per-token, no tiktoken** (`utils/string/src/truncate.rs:4`). Real numbers come from `TokenUsage` once the server replies, so client-side estimates are always coarse upper-bounds biased toward safety.
- **Per-thread, not per-turn, prompt cache key** (`client.rs:751`). Recompaction bumps `window_generation` (`client.rs:381`) to invalidate WebSocket incremental-request state but the cache key stays the same so server-side KV can still match prefixes.
- **Two-scope compaction budget**: `Total` measures absolute tokens; `BodyAfterPrefix` subtracts a per-window `prefill_input_tokens` baseline observed from server usage (`turn.rs:658-672`), letting users budget *growth* per compaction window without re-charging for the static prefix.
- **Image token estimation with byte substitution** (`history.rs:543-573`): the JSON-serialized item's base64 payload bytes are *subtracted* and replaced with a fixed per-image estimate, so prompt-cache identity-bearing bytes don't blow up local accounting.
- **Encrypted reasoning preserved across turns** via `Reasoning { encrypted_content: Some(...) }` items (`history.rs:288-296`). Non-last-turn reasoning is added to total-token estimates unless the server already accounted for it (`history.rs:312-332`).
- **Mid-turn compaction reinjects context, pre-turn does not** (`compact.rs:60-63 InitialContextInjection`). The model is trained to expect the compaction summary at the end, so mid-turn compaction splices fresh context *above* the last user message rather than appending after.
- **Reference-context-item baseline** (`history.rs:50, 81-87`): the system tracks the last "context state" snapshot it has shown the model. Steady-state turns emit only *diffs* against this baseline (`session/mod.rs:2941-2944`), which is reset by compaction or rollback.
- **Local compaction trims oldest history items on context overflow** (`compact.rs:226-232`) rather than aborting — the compaction itself is the failure-recovery for normal turn overflow.
- **Memory pipeline runs a *sub-agent* to consolidate filesystem memory** (`memories/README.md:111-117`), with `Feature::Collab` disabled to block recursive spawning.
- **Function-output truncation with 20% serialization headroom** (`history.rs:378 policy * 1.2`). The policy is applied at record time, not at request build time, so once truncated, the original output is gone.
- **`should_keep_compacted_history_item` filters remote output** (`compact_remote.rs:290-315`): drops developer messages and non-message user wrappers because remote models can echo stale instruction content.
- **Subagent reintegration via JSON-in-assistant-message** (`protocol.rs:690-698`) — clever, but couples the parent's `is_user_turn_boundary` semantics to a serialization detail.

## 5. Under-developed or Risky Areas

- **Tokenizer drift**: the 4-bytes/token heuristic can mis-estimate dramatically for non-English text, code, or short messages. Compaction triggers off this estimate before any server feedback, so a thread can blow the real window before the next turn returns server usage.
- **`session/mod.rs` is 3,339 lines** (and `session/turn.rs` is 2,142, `client.rs` is 2,245) — well over the AGENTS.md "800 LoC" guidance. Future context-handling changes have a wide blast radius. `tests.rs` is 358K (~10k+ lines).
- **Two parallel remote-compaction implementations**: `compact_remote.rs` (uses dedicated `/responses/compact`) and `compact_remote_v2.rs` (uses `/responses` with `CompactionTrigger`). Switching is feature-gated by `Feature::RemoteCompactionV2` (`turn.rs:780`). This duplication will eat maintenance until v1 is retired.
- **`COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000` and `RETAINED_MESSAGE_TOKEN_BUDGET = 64_000`** are constants in code (`compact.rs:48`, `compact_remote_v2.rs:49`); not configurable per user even though optimal values depend heavily on the active model's window.
- **Failure mode when compaction itself overflows**: the local path drops oldest items one at a time in a loop (`compact.rs:226-232`); if the trail of pruning never converges (very large recent items), the loop returns the error. The user-visible state after a failed compaction is the trimmed-but-not-summarized history (`compact.rs:230 retries = 0; continue;`).
- **`reference_context_item` correctness under rollback**: `trim_pre_turn_context_updates` (`history.rs:431-459`) explicitly clears the baseline when a mixed developer bundle is trimmed; bugs here would silently regress turns to full reinjection without warning.
- **Memory pipeline lock**: Phase 2 takes a global lock against `$CODEX_HOME/memories/` (`memories/README.md:85`); concurrent codex processes serialize through it but the lock semantics aren't documented for failure cases (stale leases on crash).
- **Heuristic image-byte adjustment LRU cache** is a fixed 32 entries (`history.rs:534 ORIGINAL_IMAGE_ESTIMATE_CACHE_SIZE`). High-image sessions will see misses.
- **Tool-output images are unconditionally replaced on invalid-image errors** (`turn.rs:368`) — recoverable, but the failure mode poisons history with a placeholder; turn-level retry has to be from the user.

## 6. Open Questions / Confidence Gaps

- **Server-side prompt cache semantics**: Codex sends `prompt_cache_key` but never inserts cache markers. The cache hit rate is observable via `TokenUsage::cached_input_tokens` but I did not read how (or whether) Codex tunes prompt construction to maximize cached prefix reuse. Worth a deeper look at `model_visible_specs()` stability across turns.
- **`Personality` and base instructions interaction**: `model_info.get_model_instructions(personality)` may bake personality into instructions (`session/mod.rs:2748-2750`). I didn't read the `models-manager` crate to confirm the exact templating; only that the session sometimes prepends a `PersonalitySpecInstructions` developer section when not baked.
- **MultiAgentV2 vs legacy agent path**: There are two distinct branches around `Feature::MultiAgentV2` (e.g., `control.rs:339,418,472`). The legacy path has a completion watcher; v2 seems to rely on inter-agent envelopes. I did not trace v2's lifecycle end-to-end.
- **Realtime conversation context**: `realtime_context.rs` (445 LoC) and `realtime_conversation.rs` (50KB) handle voice/realtime turns separately. Auto-compaction interaction was not traced — possible divergence from the text-turn assumptions in this report.
- **State DB schema details**: I confirmed migrations exist for memories and memory usage (`codex-rs/state/migrations/0006`, `0016`) but did not read SQL contents. Specific retention windows and selection rules are documented in `memories/README.md` but the precise queries weren't inspected.
- **`turn_metadata_state` header**: every request carries `turn_metadata_header.as_deref()` (`turn.rs:236`). Its exact contents (used for server-side telemetry/routing) were not inspected.
- **`compact_remote.rs:230` deletion behavior on context window exceeded retry inside local compaction**: I read the retry path but did not confirm whether `retries` is reset only on `ContextWindowExceeded` or on every transient error (`compact.rs:230-231` looks like `retries = 0; continue;` is `ContextWindowExceeded`-specific).

Confidence is high on the agent loop, compaction algorithm, history representation, tokenizer, subagent isolation/reintegration, and memory pipeline shape. It is medium on prompt-cache reuse strategy, MultiAgentV2 specifics, realtime-mode parity, and exact configuration thresholds beyond the constants I cited.
