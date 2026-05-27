# Hermes Agent — Context Management

Research report on how [`NousResearch/hermes-agent`](https://github.com/NousResearch/hermes-agent) (local checkout `/Users/season/Personal/hermes-agent`) manages context per turn, compacts long conversations, persists memory, and delegates to subagents.

All citations refer to that checkout. File sizes in this report are from a snapshot dated 2026-05-27.

---

## 1. Executive Summary

- **Primary loop lives in `agent/conversation_loop.py:263`**, not in `run_agent.py`. The huge `AIAgent` class in `run_agent.py:4154` forwards `run_conversation()` to that extracted function; `run_agent.py` is a 4.4k-LOC façade plus per-provider helpers.
- **The system prompt is built once per session and cached** (`agent/system_prompt.py:321`). It has a three-tier layout — stable, context, volatile — joined with `\n\n` (`system_prompt.py:336`). Compression is the only event that invalidates the cache (`agent/system_prompt.py:340`).
- **Prompt caching is a single strategy: `system_and_3`** (`agent/prompt_caching.py:1-9`). Up to 4 `cache_control` breakpoints (system + last 3 non-system messages), TTL `5m` or `1h`. Applied just before each API call at `conversation_loop.py:895-903`.
- **Compaction is a two-phase pipeline**: cheap tool-result pruning followed by LLM summarization (`agent/context_compressor.py:1544-1614`). Head (system + optional first-N) and tail (~tail-token-budget) are protected; middle turns are replaced by a summary message; the rebuild rotates the session into a child session in SQLite (`agent/conversation_compression.py:251`).
- **Persistence is SQLite + FTS5** (`hermes_state.py:190-307`), schema v13. Compression chains are tracked via `parent_session_id`. Cross-session recall is exposed to the agent as a dedicated tool (`tools/session_search_tool.py`), not auto-injected.
- **Memory is layered**: in-session message buffer; cross-session SQLite + FTS5; long-term markdown (`MEMORY.md` / `USER.md`) managed via `tools/memory_tool.py`; procedural memory as skills injected as user messages, not system text (`agent/skill_commands.py`).
- **Subagents are real agent instances** spawned via `tools/delegate_tool.py:1918 delegate_task`. Each child has its own `IterationBudget` (`agent/iteration_budget.py:17`), its own toolset, its own session row in SQLite, and returns only a final-summary string to the parent.
- **Token budgeting is mostly model-aware, conversation-scoped, and call-counted**. There is no global per-message-type token cap; tokens are tracked via API usage fields, and the iteration budget controls how many tool-calling rounds an agent may take.

---

## 2. End-to-End Turn Trace

### 2.1 Loop body

`AIAgent.run_conversation()` in `run_agent.py:4154` is a thin wrapper. The actual loop is `agent/conversation_loop.py:263 def run_conversation(...)`. AGENTS.md describes the loop body at a high level (`AGENTS.md:124-136`), but the real iteration code lives in that 235 KB module.

For each turn the loop, roughly:

1. **Hydrates per-turn state** — todo store rehydrated from prior messages (`conversation_loop.py:413` per the explorer trace), surrogate stripping (`_sanitize_surrogates`), non-ASCII sanitization, tool-call argument repair.
2. **Builds API kwargs** — provider-specific path (chat completions, Anthropic messages, Bedrock Converse, Codex responses, Gemini) decided at init.
3. **Injects cache breakpoints** if Anthropic-compatible:
   ```python
   # agent/conversation_loop.py:895-903
   if agent._use_prompt_caching:
       api_messages = apply_anthropic_cache_control(
           api_messages,
           cache_ttl=agent._cache_ttl,
           native_anthropic=agent._use_native_cache_layout,
       )
   ```
4. **Calls the model** via `_interruptible_api_call` or `_interruptible_streaming_api_call` (`conversation_loop.py:~1172-1176`).
5. **Dispatches tool calls** via `handle_function_call` (in `model_tools.py`), appends `assistant_msg` and tool-result messages to `messages`.
6. **Checks compression need**: `agent.context_compressor.should_compress(_preflight_tokens)` (`context_compressor.py:614`); if true, runs `compress_context` before the next iteration.
7. **Decrements iteration budget**: `IterationBudget.consume()` — and refunds for `execute_code` rounds (`agent/iteration_budget.py:37-49`).

### 2.2 System prompt assembly (per session, not per turn)

`agent/system_prompt.py:321 build_system_prompt()` calls `build_system_prompt_parts()` (line 60) and joins three tiers:

- **stable** — never changes mid-session, safe for prefix cache: persona (SOUL.md or `DEFAULT_AGENT_IDENTITY`); conditional tool guidance (`MEMORY_GUIDANCE`, `SKILLS_GUIDANCE`, plus session-search and kanban variants); Nous subscription block; provider/family operational guidance; skills prompt; environment/profile/platform hints.
- **context** — context files discovered under `TERMINAL_CWD` (AGENTS.md, CLAUDE.md, `.cursorrules`, etc.). Skipped if `skip_context_files=True`.
- **volatile** — markdown memory snapshot (`MEMORY.md`, `USER.md`), external memory-provider block, timestamp/session/model/provider line (date-only for byte stability).

Order: `stable + context + volatile`. The whole string is cached on the agent (`agent._cached_system_prompt`) and only invalidated by `invalidate_system_prompt()` (`system_prompt.py:340-348`):

```python
def invalidate_system_prompt(agent: Any) -> None:
    agent._cached_system_prompt = None
    if agent._memory_store:
        agent._memory_store.load_from_disk()
```

This is also the only place that re-reads MEMORY.md from disk, so mid-session memory writes do not enter the live system prompt until the next compression boundary or new session.

### 2.3 Tool schema assembly

Tools are built **once at agent init** via `model_tools.py:get_tool_definitions()`. Toolset enable/disable filtering and `enabled_toolsets`/`disabled_toolsets` constructor arguments are applied at init; the resulting list lives on `agent.tools`. Per turn the same list is reused, optionally with provider-specific sanitization (e.g., xAI schema stripping) inside `build_api_kwargs()`.

Discovery is driven by `tools/registry.py` and `toolsets.py` (`AGENTS.md:69-79, 264-300`). Plugin toolsets are merged in at startup.

### 2.4 Message history

`messages` is a local list inside `run_conversation()` (the explorer traced it to `conversation_loop.py:407`), not a class attribute on `AIAgent`. The list is seeded from `conversation_history` (resumed from SQLite or empty), then `messages.append(user_msg)`, `messages.append(assistant_msg)`, and tool-result appends extend it.

Persistence to SQLite happens turn-by-turn through `SessionDB` (`hermes_state.py:224` `messages` table), so a crash mid-turn loses at worst the in-flight assistant response.

### 2.5 Prompt-cache breakpoints

`agent/prompt_caching.py` is 79 lines total. The strategy comment at the top of the file is explicit:

```python
# agent/prompt_caching.py:1-9
"""Anthropic prompt caching strategy.

Single layout: ``system_and_3``. 4 cache_control breakpoints — system
prompt + last 3 non-system messages, all at the same TTL (5m or 1h).
Reduces input token costs by ~75% on multi-turn conversations within a
single session.
"""
```

The placement function (`prompt_caching.py:49-79`) deep-copies messages, marks the system message if present, then marks the last 3 non-system messages. Markers are `{"type": "ephemeral"}` plus an optional `"ttl": "1h"`. For `tool`-role messages on native Anthropic, the marker goes at the message level; for string content, content is wrapped in `[{"type": "text", "text": ..., "cache_control": ...}]`; for list content, the marker is attached to the last block (`prompt_caching.py:15-38`).

### 2.6 Provider routing

Provider mode is fixed at init (`api_mode` parameter). The branches inside `build_api_kwargs()` (paths in `agent/chat_completion_helpers.py`):

- `chat_completions` — default OpenAI-compatible path; covers OpenRouter, Nous Portal, GMI, MiMo, Moonshot, MiniMax, local LM Studio, etc.
- `anthropic_messages` — native Anthropic via `agent/anthropic_adapter.py`.
- `bedrock_converse` — AWS Bedrock via `agent/bedrock_adapter.py`.
- `codex_responses` — GitHub Copilot / xAI `/responses` endpoint via `agent/codex_responses_adapter.py`.
- `gemini_native` / `gemini_cloudcode` — Google paths via `agent/gemini_native_adapter.py`, `agent/gemini_cloudcode_adapter.py`.

All paths converge on a single transport call before re-entering the loop body.

---

## 3. Subsystem Findings

### 3.1 Compaction

Three modules with overlapping names, two of which are runtime, one is offline:

| Module | LOC | Role |
|---|---|---|
| `trajectory_compressor.py` (root) | 1508 | Offline training-data compression. README §"Research-ready" calls out "trajectory compression for training the next generation of tool-calling models." Not used by the live loop. |
| `agent/context_compressor.py` | 1749 | The actual runtime compressor. Holds threshold, summary-generation, head/tail protection. |
| `agent/conversation_compression.py` | 603 | Orchestration around the compressor — session rotation, cache invalidation, memory/context-engine hooks. |
| `agent/manual_compression_feedback.py` | 49 | Pure-function helper that formats the user-visible delta after `/compress`. |

**Trigger.** Two paths:

1. **Automatic** — after each turn, `should_compress(prompt_tokens)` is consulted (`context_compressor.py:614-634`). It compares against `self.threshold_tokens`, falls through if recent compressions have been ineffective (anti-thrashing):
   ```python
   # agent/context_compressor.py:614-634
   def should_compress(self, prompt_tokens: int = None) -> bool:
       tokens = prompt_tokens if prompt_tokens is not None else self.last_prompt_tokens
       if tokens < self.threshold_tokens:
           return False
       if self._ineffective_compression_count >= 2:
           if not self.quiet_mode:
               logger.warning(
                   "Compression skipped — last %d compressions saved <10%% each. ...",
                   self._ineffective_compression_count,
               )
           return False
       return True
   ```
2. **Manual** — `/compress [focus_topic]` slash command (`cli.py:~9758-9854` per the explorer trace) calls into `_compress_context(..., force=True, focus_topic=...)`, which bypasses the anti-thrashing cooldown.

**Algorithm.**

Phase 1 — **cheap tool-result pruning, no LLM** (`context_compressor.py:1544-1550`): old tool-result content is replaced with informative 1-line summaries (`context_compressor.py:644-649`):

```text
[terminal] ran `npm test` -> exit 0, 47 lines output
[read_file] read config.py from line 1 (3,400 chars)
```

Duplicate identical tool results are dropped except for the newest. Last `protect_last_n` (default 20) and a tail-token budget are protected from this pass.

Phase 2 — **boundary detection**: `compress_start = _protect_head_size(messages)` (system + `protect_first_n` non-system user turns, default 0 → system only) and `compress_end = _find_tail_cut_by_tokens(messages, compress_start)` walks backward to keep ~`tail_token_budget` of recent tokens.

Phase 3 — **LLM summary**: `_generate_summary(turns_to_summarize, focus_topic=...)` calls a configurable auxiliary model (default routes through OpenRouter to a Gemini Flash-class model). Budget knobs: `_MIN_SUMMARY_TOKENS=2000`, `_SUMMARY_RATIO=0.20` of compressed content, `_SUMMARY_TOKENS_CEILING=12000`. The prompt asks for Resolved/Pending questions and Remaining Work; if an earlier summary exists, it is updated iteratively.

Phase 4 — **assembly**: protected head + summary message + protected tail. The summary's role is chosen to avoid two same-role messages in a row; when forced, it is merged into the first tail message instead. The summary is wrapped with a trailing marker:

```text
\n\n--- END OF CONTEXT SUMMARY — respond to the message below, not the summary above ---
```

A short reminder is added to the system prompt at line 1636 (per explorer trace) noting that `MEMORY.md` / `USER.md` remain authoritative — the summary is *not* a license to forget memory.

**Where the summary lands.** Inline, between the protected head and the protected tail, as a standalone `{"role": "user"|"assistant", "content": <summary>}`. Original middle messages are deleted in place. The new in-memory list is returned alongside a freshly built system prompt.

**Session rotation + cache invalidation.** `conversation_compression.py:251 compress_context` calls:

```python
# agent/conversation_compression.py:371-373
agent._invalidate_system_prompt()
new_system_prompt = agent._build_system_prompt(system_message)
agent._cached_system_prompt = new_system_prompt
```

Then `agent._session_db.update_system_prompt(agent.session_id, new_system_prompt)` (`conversation_compression.py:406`) persists the new prompt, and the function returns `(compressed_messages, new_system_prompt)` (`conversation_compression.py:482`). A new session row is created with `parent_session_id` pointing at the prior session (`hermes_state.py:197, 221`), giving every compression a durable lineage.

**Failure modes.** If the aux-model summary call fails:

- Transient errors (timeout, 429, 502, 504) fall back to the main model and retry once.
- Pure provider-unavailable errors set a `_summary_failure_cooldown_until`; the auto-trigger backs off for ~10 minutes.
- If `abort_on_summary_failure=true` (configurable), the conversation freezes until the user runs `/compress` or `/new`.
- Otherwise the loop falls back to a static marker that explains the drop, deletes the middle turns anyway, and continues.

`manual_compression_feedback.summarize_manual_compression()` produces a small dict (`headline`, `token_line`, optional `note`) for the CLI to print after `/compress`, e.g. `"Compressed: 47 → 23 messages"`.

**Cache-aware policy.** Compression is explicitly framed as the only sanctioned moment to mutate the system prompt — see AGENTS.md:

```text
# AGENTS.md:866-873
The ONLY time we alter context is during context compression.
Slash commands that mutate system-prompt state (skills, tools, memory, etc.)
must be **cache-aware**: default to deferred invalidation ...
```

This is why memory-tool writes update `MEMORY.md` on disk but do **not** re-inject into the live system prompt mid-session (`agent/system_prompt.py:340-348` only fires from compression).

### 3.2 Memory

**In-session.** The `messages` list inside the loop body is the working memory. Tool calls go in as `{role: "assistant", tool_calls: ...}` and their results as `{role: "tool", ...}`. Reasoning blobs are stored in `assistant_msg["reasoning"]` and persisted to `messages.reasoning` / `reasoning_content` / `reasoning_details` columns (`hermes_state.py:235-237`).

**Cross-session — SQLite + FTS5.** `hermes_state.py:186-307` defines schema v13:

- `sessions` table (`:190-222`) with rich metadata: source, user, model, system_prompt, parent_session_id (compression lineage), timestamps, message_count, per-class token counts (`input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `reasoning_tokens`), billing, title, handoff state.
- `messages` table (`:224-242`): `session_id`, `role`, `content`, `tool_calls` (JSON), `tool_name`, `tool_call_id`, `timestamp`, `finish_reason`, `reasoning*`, `codex_reasoning_items`, `codex_message_items`, `platform_message_id`.
- `messages_fts` (`:256-258`) — FTS5 virtual table over a single `content` column.
- Trigger (`:260-266`) populates the FTS column with `COALESCE(content,'') || ' ' || COALESCE(tool_name,'') || ' ' || COALESCE(tool_calls,'')` on INSERT/UPDATE. So the FTS index is **raw concatenated text** of message content, tool name, and tool-call JSON — not LLM-generated summaries. Delete and update triggers keep it consistent.
- `messages_fts_trigram` (`:285-307`) is a parallel FTS5 table with `tokenize='trigram'` for CJK and other non-word-boundary languages.

WAL mode is attempted at open with a graceful fallback to `DELETE` mode on NFS/SMB (per explorer trace `hermes_state.py:40-161`).

**Memory tool — markdown stores.** `tools/memory_tool.py` (724 LOC). A `MemoryStore` (line 114) backs `~/.hermes/memories/MEMORY.md` (default 2200 chars) and `~/.hermes/memories/USER.md` (default 1375 chars). Entries are separated by `\n§\n`. The tool exposes `add`, `replace` (substring), and `remove` actions. Writes use a file lock plus atomic rename. The injection of these markdown contents into the system prompt happens via `MemoryStore.format_for_system_prompt()` inside the volatile tier; mid-session writes go to disk but are not reflected in the live system prompt until invalidation.

**Memory providers — pluggable cross-session backends.** `agent/memory_provider.py` defines the `MemoryProvider` ABC. `agent/memory_manager.py` (22 KB) orchestrates them:

- `initialize_all()` at agent startup.
- `prefetch_all(query)` recalls from each enabled provider before a turn; returns merged context appended into the system prompt's volatile tier.
- `queue_prefetch(query)` runs a background recall for the next turn.
- `sync_all(user_content, assistant_content)` writes a post-turn observation; non-blocking.
- `on_memory_write(...)` mirrors local memory-tool writes outward to the active provider.
- `on_pre_compress(messages)` lets providers extract insights into the compression prompt.
- `on_session_switch()` fires on `/resume`, `/branch`, `/reset`, and compression rotation.

Providers ship under `plugins/memory/<name>/` — honcho, mem0, supermemory, byterover, hindsight, and others (`AGENTS.md:515-545`). AGENTS.md:538-545 declares the in-tree provider set closed; new memory providers go in external plugins.

**Curator — "agent-curated skills with periodic nudges."** `agent/curator.py` (73 KB). Despite its size, this module is about *skill consolidation*, not pure memory eviction. Per the explorer trace, the curator maintains `.curator_state` JSON, fires when `should_run_now()` (line 199) detects inactivity beyond `interval_hours` (default 7 days), and spawns a forked review agent that ages skills (active → stale at 30 days → archived at 90 days, with `pinned` skills exempt). The reviewer is given an "umbrella-ification" prompt to merge narrow skills into class-level ones. Run reports go to `~/.hermes/logs/curator/<ts>/run.json`.

**Skills as procedural memory.** `agent/skill_commands.py` (per explorer trace lines 53-326) scans `~/.hermes/skills/<name>/SKILL.md`, builds a payload, substitutes template variables, and **injects as a user message**:

> AGENTS.md:150-151 — "Skill slash commands: `agent/skill_commands.py` scans `~/.hermes/skills/`, injects as **user message** (not system prompt) to preserve prompt caching"

This avoids cache-busting the system prompt every time a skill is invoked.

**Context files.** Read by `system_prompt.build_context_files_prompt()` from `TERMINAL_CWD` at session start, embedded into the system prompt's `context` tier. `skip_context_files=True` (constructor) suppresses them — e.g., cron and some subagent contexts.

**Cross-session recall.** Exposed as a tool the agent can call: `tools/session_search_tool.py` (24 KB). The agent receives `SESSION_SEARCH_GUIDANCE` in the system prompt when this tool is enabled (`agent/system_prompt.py:35-122`). Recall is **not auto-injected per turn** — the agent has to decide to call it. Memory-provider prefetch (`MemoryManager.prefetch_all`) is the auto-injected channel, when a provider is configured.

### 3.3 Token Budgeting

- **No central `tiktoken` use** — the agent generally relies on provider-reported token counts (the `usage` field on each response) and on character-heuristic fallbacks. The single tokenizer that does explicit token counting is the offline `trajectory_compressor.py`. The runtime compressor reads `usage.prompt_tokens` from the previous API call (`context_compressor.py:610-612`) as the input to `should_compress()`.
- **Context-length lookup** — `agent/model_metadata.py` ships hardcoded context-window sizes per model slug (longest-substring match for fallback). AGENTS.md:1120-1128 enforces an invariant: every catalog model name has an entry. `MINIMUM_CONTEXT_LENGTH` (~64 K) is the floor; anything smaller is rejected for tool-calling workloads.
- **Per-turn split** — there is no formal carve-up. The compressor's `threshold_tokens` (a ratio of context length, default ~80 %) is the only proactive cap; everything else relies on the model returning a context-overflow error if the cap is missed.
- **Truncation / backpressure** — no `MAX_TOOL_RESULT` constant. Old tool results are replaced with one-line summaries during pre-compression (`_prune_old_tool_results`), and large historical media is stripped from messages before the newest image-bearing turn (`_strip_historical_media`). Live truncation handling lives in `conversation_loop.py:1436-1645` (per explorer trace): truncated responses from quirky providers (Ollama, GLM) are detected and retried with a continuation prompt up to 3 times.
- **`IterationBudget`** (`agent/iteration_budget.py:17-62`) — *not* a token budget. It counts tool-calling rounds per agent instance: parent default 90, subagent default 50 (configurable via `delegation.max_iterations`). `execute_code` rounds are refunded so programmatic tool calls don't eat the budget:
  ```python
  # agent/iteration_budget.py:45-49
  def refund(self) -> None:
      """Give back one iteration (e.g. for execute_code turns)."""
      with self._lock:
          if self._used > 0:
              self._used -= 1
  ```
  Each subagent gets an **independent** budget (`agent/iteration_budget.py:22-26`), so a deep delegation tree can exceed the parent's nominal cap.
- **Usage reporting** — `/usage` in `cli.py` calls `agent/account_usage.py` to display rate limits (when the provider exposes them), session-cumulative token counts (including cache reads/writes and reasoning), and a cost estimate computed by `agent/usage_pricing.py`. Per-call counts also feed back into the SQLite session row (`hermes_state.py:201-217`).

### 3.4 Subagents

Subagents are real `AIAgent` instances, not lightweight async functions.

**Entry — `delegate_task` tool.** `tools/delegate_tool.py:1918`. Two modes:

- single: `goal`, optional `context`, `toolsets`, `role`.
- batch: `tasks=[{goal, context, toolsets, role}, ...]` — parallel.

**Isolation.** Each child is constructed by `_build_child_agent` (`tools/delegate_tool.py:870`). Fresh `IterationBudget` (default 50), no inherited message history, restricted toolset (the parent's delegation-related tools — `delegate_task`, `clarify`, `memory`, `send_message`, `execute_code` — are stripped by default), and a separate task ID / terminal session.

**Inheritance.** The child copies:

- model + provider + credentials (and the OpenRouter routing filters, unless overridden by `delegation.provider`).
- expanded toolset (`_expand_parent_toolsets` lets the child request sub-toolsets of the parent's composite toolset).
- MCP toolsets when `delegation.inherit_mcp_toolsets=true` (default).
- working directory and the parent's `TERMINAL_CWD`.
- reasoning config from `delegation.reasoning_effort` if set, else from the parent.
- a session-DB row linked back via `parent_session_id` for audit.

**Reintegration.** The parent never sees the child's transcript — `_extract_output_tail` (`tools/delegate_tool.py:224`) trims the result to a short summary + per-tool tail (12-entry preview kept *only* for the TUI overlay, not sent to the parent model). The parent receives a JSON-shaped tool result: `{task_index, status, summary, error, api_calls, duration_seconds, _child_role}`.

**Parallelism.** `concurrent.futures.ThreadPoolExecutor(max_workers=max_children)` (`tools/delegate_tool.py:28, 2100-2105`), where `max_children` defaults to 3 (`delegation.max_concurrent_children`). Polling uses `wait(timeout=0.5, return_when=FIRST_COMPLETED)` so parent interrupts propagate. The CLI's approval callback is forwarded via the executor's `initializer` (`tools/delegate_tool.py:59-66`). A cost-multiplier warning fires if `max_concurrent_children > 10`.

**Budget sharing.** AGENTS.md:97 says `max_iterations` is "shared with subagents." The code clarifies that this is *via inheritance of the default*, not a pooled counter — each child has its own counter capped at `delegation.max_iterations`, and parent + all children can exceed the nominal 90 (`agent/iteration_budget.py:22-26`).

**Kanban — a separate, durable orchestration model.** `plugins/kanban/` (per AGENTS.md:46) provides a SQLite-backed board with its own dispatcher loop that claims tasks and spawns assigned profiles as subagents. Boards are scoped via `HERMES_KANBAN_BOARD`, with tools `kanban_show`, `kanban_complete`, `kanban_heartbeat`, `kanban_comment`. Used for cron/batch workflows, not for live in-conversation delegation.

---

## 4. Non-Obvious Design Choices

1. **System prompt cached for life of session.** Memory writes only re-enter the live system prompt at compression or new-session boundaries (`agent/system_prompt.py:340-348`). This is a deliberate cache-cost trade-off, documented in AGENTS.md:866-873.
2. **Skills are user messages, not system text.** `agent/skill_commands.py` injects skill payloads as user turns to keep the cacheable system prefix stable. This is the inverse of what most agents do.
3. **Compression rotates the session in SQLite.** Each compaction creates a new `sessions` row with `parent_session_id` pointing at the prior one (`hermes_state.py:197, 221`; `conversation_compression.py:406`). Lineage survives forever even though message history is rewritten.
4. **FTS5 index uses raw concatenated text — content + tool name + tool calls.** No LLM summarization at index time (`hermes_state.py:260-266`). A parallel trigram FTS table covers CJK substring search (`hermes_state.py:285-307`).
5. **Two failure regimes for compression.** `abort_on_summary_failure` flips between "freeze the conversation until manual recovery" and "drop the middle anyway with a static marker." The default is graceful degradation.
6. **Independent per-subagent iteration budgets.** A tree of delegations can blow past `max_iterations`. Treat the cap as per-agent, not per-conversation.
7. **The cheap pre-pass actually does most of the work.** `_prune_old_tool_results` replaces tool-result bodies with one-line summaries and de-duplicates identical results before any LLM call. In many cases this alone gets back under threshold.
8. **Single cache layout (`system_and_3`)** rather than per-provider tuned strategies. The simplicity of the layout is the point — it works for Anthropic-native and OpenRouter-routed Anthropic both.
9. **Curator is about skills, not memory.** Despite "agent-curated memory" framing in the README, `agent/curator.py`'s consolidation logic targets the on-disk skill tree, not `MEMORY.md`. The latter is curated by the agent itself through the memory tool.
10. **Provider routing is fixed at init.** Switching `api_mode` requires a new `AIAgent` instance — there is no per-turn routing.

---

## 5. Under-Developed or Risky Areas

1. **No proactive token accounting.** The agent reacts to `last_prompt_tokens` from the previous API call to decide whether to compress *before* the next call. If a single turn balloons (large tool result, large pasted blob), the next call can still overflow; the compressor only kicks in after the model has already complained or after a turn boundary.
2. **Three modules named `*compress*`** with overlapping vocabulary. `trajectory_compressor.py` is offline-only but its name suggests runtime compression. Documentation could highlight the split better.
3. **`conversation_loop.py` is 235 KB / ~3.9 k LOC** for a single function. Per the explorer trace, line numbers like 895-903, 1172-1176, 3480 all live inside one function body. Hard to follow, hard to test in isolation, and many features (interrupt handling, streaming, provider quirks, retry, budget tracking) are tangled.
4. **Anti-thrashing kicks in silently at 2 failed passes.** `should_compress` returns False with a warning log. If the user isn't watching the log, the conversation just keeps growing until the API rejects it.
5. **Memory writes are eventually consistent.** A `memory.add` mid-session hits disk immediately, but does **not** show up in the system prompt until the next compression or new session. Surprising for users who expect "remembered" to mean "now visible to the model."
6. **Cache layout assumes Anthropic semantics.** Other providers either ignore `cache_control` markers or expose different caching APIs. `prompt_caching.py` is single-strategy by design; other providers' caches are managed at adapter layer, not visibly.
7. **Subagent transcripts disappear** unless the SQLite session row is inspected after the run. The parent has no way to revisit the child's intermediate reasoning if the summary turns out to be wrong.
8. **`messages` is a local list inside the loop**, not a class attribute. That's clean, but it means any external observer (UI, gateway hook) sees state only through the SQLite write path and the final returned dict.
9. **Schema version is 13** (`hermes_state.py:36`) — implies frequent migrations. Worth flagging for forks that pin to older versions.
10. **`prompt_caching.py` always deep-copies the full message list.** For very long histories that's a measurable per-turn cost.

---

## 6. Open Questions / Confidence Gaps

These are claims I'm less certain about because they were inferred from naming, sampling, or subagent traces I did not personally re-verify line by line.

- **Exact line for the `while`/iteration body in `conversation_loop.py`.** The explorer trace put the main loop at lines 1032-3600+ within `run_conversation()`, but I did not personally enumerate the loop construct. The function start at `:263` is verified.
- **`protect_first_n` default.** Explorer traced default to 0 (system-only head protection). The constants live in `context_compressor.py:516-517`; I did not personally read the `__init__` signature.
- **`/compress` line numbers in `cli.py`.** `cli.py` is 15 089 LOC; the `9758-9854` range came from the explorer. The behavior — `force=True`, `focus_topic` — is unambiguous from `manual_compression_feedback.py` and `conversation_compression.py`, but the exact CLI dispatch lines were not double-checked.
- **Auxiliary-model default.** Explorer claimed "Gemini Flash via OpenRouter" as the compression summarizer default. I did not personally trace `compression` config defaults in YAML; I'm taking that on trust.
- **Curator call site.** I did not find the exact line where `curator.should_run_now()` is invoked. It is likely in `cli.py` or `gateway/run.py` per the explorer guess, but unverified.
- **Honcho prefetch behavior.** The `MemoryManager.prefetch_all` flow is general; the *specific* behavior of any individual provider (honcho, mem0, supermemory) was not traced.
- **`/usage` exact output format.** Confirmed it calls into `account_usage.py` and `usage_pricing.py`, but did not personally render its output.
- **Whether the system-prompt rebuild inside `compress_context` (`conversation_compression.py:371-373`) also runs `MemoryStore.load_from_disk()`.** `invalidate_system_prompt` does (`system_prompt.py:340-348`), and `_invalidate_system_prompt` is called first, so it should — but the wrapper on `AIAgent` might short-circuit. Not personally verified.
- **Truncation-detection logic** in `conversation_loop.py:1436-1645` — the explorer trace described retry-with-continuation for Ollama/GLM; I did not personally read those lines.
- **`messages` local variable** — confirmed by the explorer at `conversation_loop.py:407`; I did not personally open that range.

---

## Citation Index

Verified personally (read or grepped during this session):

- `agent/iteration_budget.py:1-62` — full file, IterationBudget defaults 90/50.
- `agent/prompt_caching.py:1-79` — full file, single `system_and_3` strategy.
- `agent/conversation_loop.py:263` — `def run_conversation(`.
- `agent/conversation_loop.py:64, 895-903` — `apply_anthropic_cache_control` import and call site.
- `agent/system_prompt.py:60, 321, 336, 340-348` — three-tier builder, `invalidate_system_prompt`.
- `agent/context_compressor.py:500-571, 614-634, 1546, 1585, 1591` — threshold setup, should_compress.
- `agent/conversation_compression.py:251, 371-373, 406, 482` — entry, invalidate + rebuild, persist, return.
- `agent/manual_compression_feedback.py` — 49 LOC file, confirmed.
- `hermes_state.py:36, 186-307` — schema v13, sessions/messages tables, FTS5 + trigram FTS5.
- `tools/delegate_tool.py:28, 224, 870, 1321, 1918, 2061, 2094, 2105` — ThreadPoolExecutor, `_extract_output_tail`, `_build_child_agent`, `_run_single_child`, `delegate_task`, single/batch dispatch.
- `tools/{memory_tool.py,session_search_tool.py,todo_tool.py,delegate_tool.py}` — existence + sizes.
- `AGENTS.md:32, 46, 86-138, 150-151, 305-307, 513-545, 707, 719-741, 800-851, 866-873, 1118-1128` — architectural commentary.
- `README.md:15-27` — feature list, "Spawn isolated subagents," "FTS5 session search with LLM summarization."

Inherited from Explore agent traces (not personally re-read line by line):

- `agent/system_prompt.py:90-253` — stable-tier composition details.
- `agent/context_compressor.py:1086-1714` — summary generation, assembly, role choice, failure handling.
- `agent/memory_manager.py:244-609` — lifecycle hooks.
- `agent/curator.py:8-471` — curator state, intervals, umbrella-ification.
- `tools/memory_tool.py:60, 114, 125, 271-274, 444-455, 603-641` — MemoryStore internals.
- `agent/skill_commands.py:53-326` — skill scan and user-message injection.
- `cli.py:9758-9854, 9915-` — `/compress`, `/usage` CLI handlers.
- `agent/model_metadata.py:139-229` — context-length table.

---
