# Kimiflare Context Management

Path researched: `/Users/season/Personal/kimiflare` (TypeScript / Node.js ≥ 20, Ink TUI, Cloudflare AI Gateway).

## 1. Executive summary

Kimiflare is a multi-provider terminal coding agent routed through Cloudflare AI Gateway. Its context window is held entirely in-memory as a single mutating `ChatMessage[]` array, owned by the Ink TUI (`messagesRef` in `src/app.tsx`) and mutated in place by the loop in `src/agent/loop.ts`. There is one primary agent loop — `runAgentTurn` — driven by a `TurnSupervisor` that wraps it for the UI; smaller variants exist for `/init` and the headless SDK but share the same loop function.

The design has five context-management subsystems:

1. **Cache-stable dual system prompt** (a two-message system prefix where index 0 is byte-stable and index 1 changes only when mode/tools/skills/KIMI.md change).
2. **Tool-output reduction + an artifact store** that swap large tool results for short summaries plus an artifact id (`expand_artifact` can pull the full bytes back).
3. **Compaction** — two strategies: an LLM summarizer (`/compact` default) and an "artifact compaction" path that buckets older turns into a structured `SessionState` plus an artifact index (opt-in via `cfg.compiledContext`). Compaction is also wired into `runAgentTurn` via `onIterationEnd` so it can fire automatically between tool iterations.
4. **Cross-session memory** in SQLite with embeddings (`src/memory/`), recalled at session start, after compaction, and on demand via tools.
5. **Token budgeting via a `chars/3.5` heuristic** (no real tokenizer) used to (a) hard-error before the prompt would exceed the model's context window and (b) decide when to auto-compact.

There is **no subagent / multi-agent isolation primitive**. The closest thing is "Code Mode," where a heavy-tier turn replaces all OpenAI-style tools with a single `execute_code` tool that runs LLM-generated TypeScript in an `isolated-vm` (or `node:vm` fallback) sandbox; that sandbox shares the same `ToolExecutor` and tool-permissions plumbing — not a separate conversation.

Tokenization is approximate (`chars / 3.5`); the only "true" usage numbers come from the model's streamed `usage` field and Cloudflare AI Gateway's per-request logs. Prompt-cache "breakpoints" in the sense of Anthropic's `cache_control` are **not** implemented — the design assumes a generic provider-side prefix cache and protects it by keeping `messages[0]` byte-identical for the whole session.

## 2. End-to-end turn trace

The trace below follows a single user prompt entered into the TUI (the most-trafficked path). File references are absolute `path:line`.

### 2.1 Prompt assembly (session bootstrap)

At app construction in `src/app.tsx:383-386` the messages array is initialised once:

```ts
const cacheStableRef = useRef(initialCfg?.cacheStablePrompts !== false);
const messagesRef = useRef<ChatMessage[]>(
  makePrefixMessages(cacheStableRef.current, cfg?.model ?? DEFAULT_MODEL, "edit", ALL_TOOLS),
);
```

`makePrefixMessages` (`src/ui/app-helpers.ts:289-304`) returns either one combined system message (`buildSystemPrompt`) or two (`buildSystemMessages`) depending on `cacheStablePrompts`. The dual-message form is documented in `src/agent/system-prompt.ts:46-54`:

```
NOTE: this prefix MUST NOT contain the model name. In cache-stable mode the
loop only refreshes messages[1] (the session prefix) per turn — messages[0]
is set once at session start and never updated.
```

`buildStaticPrefix` (`src/agent/system-prompt.ts:54-77`) holds identity, "how to work", and tool-output-reduction rules — invariant text. `buildSessionPrefix` (`src/agent/system-prompt.ts:81-127`) emits the model identity line, env block (cwd, platform, shell, home, today's date), enumerated tool list, optional LSP guidance, the contents of any `KIMI.md`/`KIMIFLARE.md`/`AGENT.md` (capped at 20 KB, `system-prompt.ts:21,30-43`), mode rules, and a `skillContext` block.

### 2.2 User submits a prompt

`processMessage` in `src/app.tsx` (~line 1480+) runs intent classification once, pushes the user `ChatMessage` into `messagesRef.current` (`src/app.tsx:1535`), then optionally recalls compiled-context artifacts (`src/app.tsx:1540-1551`) before handing the buffer to `supervisorRef.current.startTurn(...)` (`src/app.tsx:1776-1813`).

Intent classification (`src/intent/classify.ts:20-52`) returns `{intent, rawScore, tier, confidence}` where `tier ∈ {light, medium, heavy}`. The TUI reuses `tier` for: (a) reasoning effort selection, (b) whether to enable Code Mode (`tier === "heavy"` → `effectiveCodeMode = true`, `src/app.tsx:1584-1585`), and (c) the skill-routing budget.

### 2.3 The supervisor

`TurnSupervisor.startTurn` (`src/agent/supervisor.ts:38-73`) is a thin lifecycle wrapper:

```ts
this.currentTurn = runAgentTurn(opts)
  .then(async () => { this._phase = "idle"; … })
  .catch(async (error) => { this._phase = "idle"; … })
```

It refuses to start a new turn if one is already running (`supervisor.ts:39-44`) and records a `_killRequested` flag for interruptions. It does **not** copy messages — the array is shared by reference.

### 2.4 Pre-turn work inside `runAgentTurn`

`runAgentTurn` (`src/agent/loop.ts:255-1151`) is the primary loop. Before the first model call it:

1. **Computes a "fire-and-forget" Stop hook** (`loop.ts:265-277`) — invoked from each clean-exit return site.
2. **Decides whether to skip skill routing** (`loop.ts:286-293`):

   ```ts
   const skipSkillRouting =
     opts.intentClassification?.tier === "light" &&
     lastUserPrompt.length < 40;
   ```

3. **Runs memory recall and skill routing in parallel** (`loop.ts:295-326`). `sessionStartRecall` was kicked off at startup in `src/ui/run-startup-tasks.ts:135`:

   ```ts
   sessionStartRecallRef.current = manager.recall({ text: cwd, repoPath: cwd, limit: 5 });
   ```

   Inside the loop, the recall promise is awaited only on the first turn (the ref is nulled after consumption in `src/app.tsx:1766`) and the result is synthesised into prose by a lightweight "plumbing" model (`MemoryManager.synthesizeRecalled`, `src/memory/manager.ts:370-386`).
4. **Injects recall + skills into the system block** (`loop.ts:339-376`):

   ```ts
   if (recallSettled.status === "fulfilled" && recallSettled.value) {
     const { text, count } = recallSettled.value;
     const lastSystemIdx = opts.messages.findLastIndex((m) => m.role === "system");
     const insertIdx = lastSystemIdx >= 0 ? lastSystemIdx + 1 : opts.messages.length;
     opts.messages.splice(insertIdx, 0, { role: "system", content: text });
   }
   if (skillsSettled.status === "fulfilled" && skillsSettled.value) {
     …
     if (opts.cacheStable) {
       opts.messages[1] = { role: "system", content: buildSessionPrefix({ … skillContext: skillResult.skillContext }) };
     } else {
       opts.messages[0] = { role: "system", content: buildSystemPrompt({ … skillContext: skillResult.skillContext }) };
     }
   }
   ```

   The cache-stable branch deliberately mutates only `messages[1]`, leaving `messages[0]` byte-identical — see also the doc comment quoted in §2.1.

### 2.5 Building tools and the request body

If `codeMode` is on, `runAgentTurn` swaps `toolDefs` for a single `execute_code` function whose description embeds the generated TypeScript API for all real tools (`loop.ts:393-431`). Otherwise `toolDefs = toOpenAIToolDefs(opts.tools)`.

`apiMessages` (the array actually sent) is built from `opts.messages` with two optional transforms:

- **Historical reasoning strip** (`KIMIFLARE_STRIP_REASONING=1`, `loop.ts:523-552`) calls `stripHistoricalReasoning` (`src/agent/strip-reasoning.ts`). Rules (verbatim, `strip-reasoning.ts:11-21`):

  > - Keep reasoning_content on the N most recent assistant messages (default N=1).
  > - On older assistant messages:
  >   - Delete reasoning_content key entirely.
  >   - If the message has both text content and tool_calls, replace text with "".
  >   - If the message has only text (no tool_calls), preserve it only when text length > SUBSTANTIVE_TEXT_THRESHOLD chars; otherwise replace with "".
  > - tool, system, and user messages are never modified.

  `SUBSTANTIVE_TEXT_THRESHOLD = 200` (`strip-reasoning.ts:9`). A shadow-strip mode (`KIMIFLARE_SHADOW_STRIP=1`) measures savings without applying the strip.

- **Old image stripping** (`loop.ts:554-556`) calls `stripOldImages` (`src/agent/messages.ts:100-130`) to remove `image_url` parts from user messages older than `keepLastImageTurns` (default 2 turns, set from `cfg.imageHistoryTurns ?? 2` in `src/app.tsx:1796`).

### 2.6 Token-budget pre-flight

Right before each request (`loop.ts:558-570`):

```ts
const promptTokens = estimatePromptTokens(apiMessages);
const ctxWindow = getModelOrInfer(opts.model).contextWindow;
const completionBudget = opts.maxCompletionTokens ?? DEFAULT_MAX_COMPLETION_TOKENS;
const maxPromptTokens = ctxWindow - completionBudget - BUDGET_SAFETY_MARGIN_TOKENS;
if (promptTokens > maxPromptTokens) {
  throw new Error(`kimiflare: context window exceeded (~${promptTokens.toLocaleString()} / …) … Run /compact …`);
}
```

`estimatePromptTokens` (`src/agent/artifact-compaction.ts:42-64`) is the `chars/3.5` heuristic (chosen to over-estimate vs the server tokenizer — `artifact-compaction.ts:36-44`). `DEFAULT_MAX_COMPLETION_TOKENS = 16_384` and `BUDGET_SAFETY_MARGIN_TOKENS = 8_192` are loop constants (`loop.ts:218-223`).

### 2.7 Streaming request

`runKimi` (`src/agent/client.ts:82-224`) builds the request URL (direct Workers AI vs AI Gateway `/compat/chat/completions`) and POSTs with `stream: true`. For non–Workers AI providers the model goes in the body as `compatModel = "workers-ai/" + opts.model` or plain `<provider>/<model-id>` (`client.ts:97-100`).

Gateway-specific request shaping is at `client.ts:577-589` and `client.ts:265-282`:

```ts
const turnGateway = opts.gateway
  ? { …opts.gateway,
      metadata: { …(opts.gateway.metadata ?? {}),
        feature: "chat",
        ...(opts.sessionId ? { sessionId: opts.sessionId } : {}),
        tier: opts.intentClassification?.tier ?? "medium",
        cm: codeMode ? "1" : "0",
        skl: String(skillResult?.sectionCount ?? 0),
      } }
  : undefined;
```

The metadata is shipped as the `cf-aig-metadata` header (capped at 5 keys, `client.ts:277-281`). Cache-related headers are `cf-aig-cache-ttl`, `cf-aig-skip-cache`, `cf-aig-collect-log-payload` (`client.ts:267-277`). These tune **Cloudflare AI Gateway's response cache**, not any Anthropic-style prompt-cache breakpoint — the loop never sends `cache_control` blocks.

### 2.8 Streaming events → tool dispatch

`parseStream` (`client.ts:373-461`) yields typed `KimiEvent`s: `gateway_meta`, `reasoning`, `text`, `tool_call_start`, `tool_call_args`, `tool_call_complete`, `usage`, `done`. `runAgentTurn` consumes them in `loop.ts:610-652`, accumulating text/reasoning into the in-flight assistant message and collecting tool calls into `toolCalls[]`.

Once the stream ends, the assistant message is built and pushed (`loop.ts:673-690`). If `toolCalls.length === 0`, the turn returns (after logging and firing `Stop`).

When tool calls are present, the loop iterates them (`loop.ts:719-1083`) and for each one applies, in order:

1. **Anti-loop signature check** (`loop.ts:723-746`). A signature `name:stableStringify(args)` is appended to `recentToolCalls` (window 8, threshold 2 — `loop.ts:438-439`). On the third identical call, a synthetic failure message is pushed instead of executing the tool.
2. **Web-fetch spiral guardrail** (`loop.ts:748-828`): per-turn cap (5), per-domain threshold (2 — i.e. third hit blocks), and a session-wide cap of 25 (`loop.ts:181-183, 446-447`). The session map is module-level and indexed by `sessionId`.
3. **Code Mode branch** (`loop.ts:830-894`): `runInSandbox` is invoked, individual `SandboxToolCall`s are emitted as `tool_result` callbacks, and the combined sandbox output (capped to `MAX_TOOL_CONTENT_CHARS = 10_000`, `loop.ts:227, 866-877`) becomes the single tool message.
4. **Normal branch** (`loop.ts:895-1082`): `executor.run(...)` is called. The executor (`src/tools/executor.ts:208-372`) fires `PreToolUse` (veto-able, `executor.ts:246-275`), prompts for permission if the tool requires it, runs the tool, then passes the result through `reduceToolOutput` (which produces the short summary + artifact id pair — see §3.3). Diff-style git commands are exempted from reduction (`executor.ts:319-335`).
5. **Memory auto-extraction** (`loop.ts:967-1077`): for each extractor in `src/memory/extractors.ts` whose `match()` returns true, an IIFE runs the extractor asynchronously; on success it calls `memoryManager.remember(...)` and feeds the sliding-window drift detector for `KIMI.md` staleness (window 10, threshold 3 — `loop.ts:159-161`).

Tool messages with content longer than `MAX_TOOL_CONTENT_CHARS = 10_000` are truncated in-place (`loop.ts:931-943`), with an `onTruncation` callback emitted so the UI can show the artifact id.

### 2.9 Inter-iteration compaction

After all tool results have been pushed, `onIterationEnd` runs (`loop.ts:1093-1096`). The TUI's implementation (`src/app.tsx:750-849`) calls `shouldCompact({messages})` (`src/agent/artifact-compaction.ts:265-275`), and if true runs one of:

- **`compactMessagesViaArtifacts`** when `cfg.compiledContext === true` (default off, `src/app.tsx:404`),
- **`summarizeMessagesViaLlm`** otherwise.

`shouldCompact` triggers at `tokens > 80_000` or `turns > 12` (defaults — `artifact-compaction.ts:270-274`).

### 2.10 Loop termination

`runAgentTurn` loops back to the top of `while (true)` and starts another turn iteration. It exits when:

- The assistant responds with no tool calls (clean exit, `loop.ts:692-717`).
- `budgetExhausted` is set (cumulative `prompt_tokens` ≥ `opts.maxInputTokens`) — one final "synthesize" turn runs, then `BudgetExhaustedError` is thrown (`loop.ts:457-471, 711-713, 1117-1119`).
- `loopExhausted` is set (every tool call in the iteration was blocked by a guardrail) — the loop either offers the user a continue/synthesize/stop choice via `onLoopDetected` or throws `AgentLoopError` (`loop.ts:1085-1149`).
- `iter >= max` (default 50, `loop.ts:258, 475-501`) — calls `onToolLimitReached` for continue/stop or throws.
- The abort signal trips — `DOMException("aborted")` is thrown.

### 2.11 After-turn persistence

When the supervisor's `onDone` fires the TUI calls `saveSessionSafe()`, which writes the entire messages array plus `sessionState` and serialized artifact store to disk under `$XDG_DATA_HOME/kimiflare/sessions/<id>.json` (`src/sessions.ts:51-101`). Across sessions, only the SQLite memory DB persists semantic state.

## 3. Subsystem findings

### 3.1 Per-turn request assembly

| Slot | Source | Cache property |
|---|---|---|
| `messages[0]` (cache-stable on) | `buildStaticPrefix` — identity, "how to work", reduction rules | byte-identical for the session |
| `messages[1]` (cache-stable on) | `buildSessionPrefix` — env, tools, KIMI.md, mode rules, skill context | refreshed on every turn but only when contents change (mode/tools/skills/model) |
| Recalled memory system msg | inserted after the last system msg by `loop.ts:339-346` on first turn only | added once per session |
| User / assistant / tool messages | mutated in place by the loop | replayed every turn until compaction |
| `toolDefs` | `toOpenAIToolDefs` or the single `execute_code` def (Code Mode) | refreshed per turn, but stable when toolset is unchanged |
| Body params | `temperature` default `0.2`, `max_completion_tokens` `16384`, `reasoning_effort` if model supports it, `parallel_tool_calls: true` (`client.ts:101-122`) | per-turn |

There are **no explicit prompt-cache breakpoints** in the request body (no `cache_control` on system/tool/user blocks). The strategy is "keep the prefix byte-stable and let the gateway/upstream cache it." Quote from `system-prompt.ts:46-54`:

> Build the truly static prefix that should remain byte-for-byte identical across all turns in a session. Contains identity and invariant rules only.

The closest related machinery is the AI Gateway *response* cache (key the entire request → reuse a previous response): `cf-aig-cache-ttl`, `cf-aig-skip-cache` (`client.ts:267-273`). That is a different abstraction from prompt prefix caching.

### 3.2 Tool definitions

`ALL_TOOLS` (`src/tools/executor.ts:20-37`) is the canonical list. MCP and LSP tools are appended at runtime in `src/app.tsx:1783` and similar SDK paths. Each `ToolSpec` advertises `needsPermission`, a JSON schema (used by `toOpenAIToolDefs`), and an optional `render(args)` hint for the TUI.

### 3.3 Tool-output reduction and the artifact store

The executor reduces large outputs (`executor.ts:337-352`) via `reduceToolOutput` with per-tool caps in `DEFAULT_REDUCER_CONFIG` (`src/tools/reducer.ts:50-94`) — e.g. `grep` is capped at 50 total lines / 3 matches per file / 3 000 chars, `read` at 200 slice lines / 4 000 chars, `bash` at 40 lines / 4 000 chars with consecutive-line de-dupe, `web_fetch` at 2 000 chars. The full raw bytes are stored in `ToolArtifactStore` (`src/tools/artifact-store.ts`) and recoverable via the auto-registered `expand_artifact` tool (`executor.ts:177-178`). The static system prompt explicitly tells the model about this (`system-prompt.ts:73-76`):

```
- Large tool outputs (grep, read, bash, web_fetch) are reduced to compact summaries by default to preserve context window.
- When you see "[output reduced]" with an artifact ID, you can call `expand_artifact` with that ID to retrieve the full raw output if you need more detail.
```

Reduction happens **before** the result message enters `opts.messages`. The agent loop's separate `MAX_TOOL_CONTENT_CHARS = 10_000` cap (`loop.ts:227`) is a second layer that truncates whatever survived reduction.

### 3.4 Compaction

There are two implementations and a shared trigger.

**Trigger** — `shouldCompact` (`artifact-compaction.ts:265-275`):

```ts
const tokenThreshold = opts.tokenThreshold ?? 80_000;
const turnThreshold = opts.turnThreshold ?? 12;
return tokens > tokenThreshold || turns.length > turnThreshold;
```

Wired into `onIterationEnd` (`src/app.tsx:752`) and into the `/compact` slash command (`src/agent/run-compact.ts:53-148`).

**Algorithm A — `compactMessagesViaArtifacts`** (`artifact-compaction.ts:281-355`):

1. `groupIntoTurns` splits the array into a leading `prefix` of system messages plus `Turn = { user, assistant, tools[] }` groups (`artifact-compaction.ts:68-99`).
2. The last `keepLastTurns` (default 4) raw turns are kept; everything older is archived.
3. Each archived turn is run through `extractArtifactsFromTurn` (`artifact-compaction.ts:106-237`) which, depending on tool name, builds an `Artifact` (typed `read_slice | bash_log | grep_result | web_fetch | tool_result | assistant_decision`) plus a `stateDelta`. The delta merges into the running `SessionState` (`artifact-compaction.ts:239-262`).
4. The resulting structured state is serialised as a single `[compiled session state]` system message (`session-state.ts:211-239`) and inserted in place of the dropped raw turns.
5. After compaction the TUI also runs `memoryManager.recall` against the current task and injects a synthesised system block (`src/app.tsx:792-815`).

`SessionState` fields kept across compaction (`session-state.ts:28-51`): `task`, `user_constraints`, `repo_facts`, `files_touched`, `files_modified`, `confirmed_findings`, `open_questions`, `recent_failures`, `decisions`, `next_actions`, `artifact_index`. Dropped: raw assistant narration, raw tool message bodies (preserved indirectly via `ArtifactStore`).

`ArtifactStore` (`session-state.ts:69-149`) caps at 200 artifacts / 500 000 chars total. Eviction is "size-weighted over the oldest quartile" (`session-state.ts:138-148`):

```
Bounded by the oldest quartile so we never evict freshly-added artifacts;
size-weighted within that window so one big artifact gets dropped instead
of many small ones.
```

**Algorithm B — `summarizeMessagesViaLlm`** (`src/agent/llm-summarize.ts:44-117`):

1. Keep all leading system messages as `prefix`.
2. Keep the last `keepLastTurns` (default 4) user-message-anchored turns.
3. Render everything between as a `[role] content (tool_calls: …)` transcript, truncating tool content to 500 chars.
4. Call `runKimi` with a short summary system prompt aimed at "~400-800 tokens" (`llm-summarize.ts:21-28`), `temperature: 0.1`, `reasoningEffort: "low"`.
5. Insert the result as a single `user` message prefixed with `[compacted summary of earlier turns]` — note: this is a `user` message, not `system`, intentionally so the original system prefix stays cache-stable (`llm-summarize.ts:106-115`).

If `messagesRef` doesn't contain a leading system message, summarization is skipped (`llm-summarize.ts:53-57`).

**Preserved vs dropped**: in both paths, leading system messages, the last 4 user-anchored turns, and (for Algorithm A) tool artifacts + structured state survive. Dropped: raw older assistant prose, raw older tool bodies (in Algorithm A, only summaries plus `ArtifactStore` raw bytes remain).

### 3.5 In-session state

- `messagesRef.current` — the running `ChatMessage[]`.
- `sessionStateRef.current` — `SessionState` populated only when `compiledContext` is on.
- `artifactStoreRef.current` — `ArtifactStore` (compiled-context only).
- `recentToolCalls` (per-turn module-level slice, `loop.ts:437`) and `sessionWebFetchHistory` (per-session map, `loop.ts:181`) — anti-loop tracking.
- `driftEvents` (module-level map keyed by sessionId, `loop.ts:159`) — sliding window for `onKimiMdStale`.
- `memoryExtractionErrorCounts` (`loop.ts:169`) — per-session counter for `/memory health` surface.

### 3.6 Cross-session persistence

**Sessions** are persisted to disk (`src/sessions.ts:51-101`): `$XDG_DATA_HOME/kimiflare/sessions/<id>.json`. The on-disk record (`SessionFile`, `sessions.ts:34-49`) carries `messages`, optional `sessionState`, optional `artifactStore`, and optional `checkpoints`. `/resume` reloads any prior session. Retention is governed by `RETENTION` constants (`src/storage-limits.ts`) and pruned at startup.

**Memory** lives in SQLite at `.kimiflare/memory.db` (project-local) or `~/.config/kimiflare/memory.db` (global). Schema in `src/memory/schema.ts:3-20`:

```ts
export interface Memory {
  id: string;
  content: string;
  embedding: Float32Array;
  category: MemoryCategory; // "fact" | "event" | "instruction" | "task" | "preference"
  sourceSessionId: string;
  repoPath: string;
  createdAt: number;
  accessedAt: number;
  importance: number;
  relatedFiles: string[];
  topicKey: string | null;
  supersededBy: string | null;
  forgotten: boolean;
  vectorized: boolean;
  agentRole: string | null;
}
```

Embeddings default to `@cf/baai/bge-base-en-v1.5` (768 dims, `schema.ts:62`). `MemoryManager.remember` (`src/memory/manager.ts:200-273`) does: secret redaction → LLM verify → topic-key normalization → supersession of same-topic memories → hypothetical-query expansion for embed text → embed + insert.

Recall (`manager.ts:325-344`) embeds the query and runs the hybrid retrieval pipeline (`src/memory/retrieval.ts`, FTS + vector + exact + topic-key signals). Session-start recall uses the cwd as the query text (`run-startup-tasks.ts:135`), so it returns repo-scoped memories.

Memory is also recallable mid-turn via the `memory_recall` tool and writable via `memory_remember` / `memory_forget` (`src/tools/memory.ts`, registered in `ALL_TOOLS`).

Auto-extraction runs on every tool result (`loop.ts:967-1077`) using deterministic extractors in `src/memory/extractors.ts` (mostly regex/JSON-parse, one LLM-based edit-event synthesizer).

### 3.7 Token budgeting

- **Tokenizer**: there isn't one. `approxTokens(chars) = round(chars / 3.5)` (`artifact-compaction.ts:36-44`). A separate `chars / 4` heuristic is used for cost-debug logging (`src/cost-debug.ts:93-96`) — distinct from the loop's hard-fail estimator. There is **no `tiktoken` / `bge` / model-specific tokenizer** anywhere in `src/`.
- **Per-turn split**: there is no fixed split (system / tools / history). The loop computes one number, `promptTokens`, vs `maxPromptTokens = ctxWindow - completionBudget - 8192` and errors before sending if exceeded.
- **Truncation / back-pressure ladder**: (i) reducer caps each tool output (per-tool), (ii) loop caps each tool message at 10 000 chars, (iii) `keepLastImageTurns` strips old images, (iv) optional `stripHistoricalReasoning` strips older reasoning + narration, (v) `shouldCompact` triggers compaction at 80k tokens or 12 turns, (vi) `BudgetExhaustedError` fires when cumulative input ≥ `maxInputTokens` (configured by caller, e.g. print mode exits 42 — `loop.ts:139-144` and README:164).
- **Per-model context** is read from `getModelOrInfer(opts.model).contextWindow` (`src/models/registry.ts`).

### 3.8 Subagents

**Not present.** Searches for `subagent`, `sub-agent`, `child_session`, `spawnAgent`, `createSubAgent`, `subAgent` returned zero matches in `src/`. The only "sub-execution" primitive is Code Mode's sandbox (`src/code-mode/sandbox.ts`) where LLM-generated TypeScript runs in `isolated-vm` (or `node:vm` fallback) and calls back into the same `ToolExecutor` instance; this is not a separate conversation, system prompt, or message history. Tool calls inside the sandbox still go through `PreToolUse`/`PostToolUse` hooks via the executor (`executor.ts:241-275, 381-410`). There is therefore no subagent isolation, inheritance, or reintegration to describe.

The headless SDK (`src/sdk/session.ts`) creates a separate top-level `KimiFlareSession` rather than a child of an existing one; each session has its own message buffer, executor, and (optionally) memory manager.

## 4. Non-obvious design choices

- **Two system messages instead of one + `cache_control`** (`system-prompt.ts:46-54`). The cache strategy is provider-agnostic — it relies on whatever prefix cache the upstream uses and protects the prefix by never editing `messages[0]` after session start. Re-running `/model` mid-session would silently leave a stale model name in `messages[0]` if identity lived there; identity therefore lives in `messages[1]` and `messages[0]` is *intentionally model-agnostic*.
- **Compaction summaries are inserted as `user` messages**, not `system`. Quote from `llm-summarize.ts:106-115`: the summary message is `{role: "user", content: "[compacted summary of earlier turns]\n…"}`. This preserves the leading system-block prefix.
- **`compiledContext` is opt-in** (`src/app.tsx:404` — `useRef(initialCfg?.compiledContext === true)`). The default `/compact` path is the LLM summarizer; the artifact-based path is the more elaborate but less proven alternative.
- **Anti-loop guardrails are persistent across turns by `sessionId`** (`loop.ts:179-198`). A research spiral that splits a web-fetch loop across 5 turns is still caught by the per-session 25-fetch cap. Drift events for KIMI.md staleness are also keyed by `sessionId`.
- **`MAX_TOOL_CONTENT_CHARS = 10_000`** in the loop is the second line of defence; the reducer already capped per-tool outputs separately. The loop's truncation explicitly surfaces an artifact id so the model can `expand_artifact`. Diff-style git commands are exempted from reduction (`executor.ts:319-335`) because line-dedupe mangles diff output and traps the model in retry loops — a learned hazard.
- **Estimator over-counts deliberately** (`artifact-compaction.ts:36-44`): chars/3.5 vs the typical chars/4 quote-of-thumb, on the theory that "code- and JSON-heavy content tokenizes denser." This biases compaction and budgeting toward firing earlier than strictly needed.
- **Code Mode swaps the entire tool surface for a single sandbox tool** when intent classifier rates the prompt `heavy`. The TypeScript API string is cached per-toolset hash (`codeModeApiCache`, `loop.ts:153, 394-401`) to avoid re-generating it every turn.
- **Memory `remember()` issues three secondary LLM calls** (verify, topic-key normalization, hypothetical-query expansion — `manager.ts:218-273`) using a small "plumbing" model (`@cf/moonshotai/kimi-k2.5` by default) before persisting. That is a deliberate cost trade for higher recall quality.
- **`stableStringify` deterministic key-sorting** (`messages.ts:82-98`) is used everywhere tool arguments need to be compared (anti-loop signatures) or sent in a body (cache-key friendliness).
- **`sanitizeString` replaces lone UTF-16 surrogates with U+FFFD** at every assistant/tool boundary (`messages.ts:46-54`). The doc comment explains why: a single bad model token could otherwise "permanently poison the conversation history."

## 5. Under-developed or risky areas

- **No real tokenizer.** Every budget decision uses `chars / 3.5` or `chars / 4`. For very code- or JSON-heavy turns the estimator may under-count enough that the request still 4xx's at the gateway. There's a `BUDGET_SAFETY_MARGIN_TOKENS = 8_192` cushion (`loop.ts:222`), but no fallback if a real provider tokenizer would have disagreed.
- **Compaction quality is not load-tested.** `compactMessagesViaArtifacts` uses regex patterns to harvest "decisions" from assistant prose (`artifact-compaction.ts:220-234`):

  ```ts
  const decisionPatterns = [
    /(?:decided?|will|plan to|going to|should|need to)\s+(.{10,200})/gi,
    /(?:let's|let us)\s+(.{10,200})/gi,
  ];
  ```

  False positives here will leak into `SessionState.decisions` forever (deduplicated but not pruned).
- **`ArtifactStore` is in-memory only**, with serialization to the session file as an opt-in. If `compiledContext` is on but the session isn't saved before a crash, all archived raw tool outputs are lost. Re-running the same tools is the only recovery path.
- **Pre-turn parallelism swallows non-abort errors silently** (`loop.ts:328-337`):

  ```ts
  for (const settled of [recallSettled, skillsSettled]) {
    if (settled.status === "rejected" && settled.reason instanceof DOMException && settled.reason.name === "AbortError") {
      throw settled.reason;
    }
  }
  ```

  A bad embedding response or DB lock during session-start recall will produce zero recall with no user-visible signal beyond a debug log.
- **Anti-loop guardrails use exact-match argument signatures.** A model that varies one whitespace character per call escapes detection. The signature `stableStringify` mitigates key-order jitter but not value jitter.
- **Mid-turn compaction inserts the synthesised memory recall after compaction** (`src/app.tsx:792-815`) but does so via `array.splice`, which can race with the loop pushing further messages onto the same ref. In practice the loop awaits `onIterationEnd` before continuing, so it's safe — but the safety is by serial discipline, not isolation.
- **The intent classifier is regex-based** (`src/intent/classify.ts:8-18`). It will mis-tier non-English prompts and any task description that doesn't match its hard-coded verbs/nouns.
- **No subagent / parallel-research primitive.** For tasks that would benefit from fan-out (e.g. read-ten-files-and-summarize), the loop fans them out via `parallel_tool_calls: true` (`client.ts:104`) but every result lands in the same flat history; there is no way to isolate research into a separate context and merge a summary back.
- **`KIMI.md` cap is 20 KB** (`system-prompt.ts:21`). Larger context files are silently dropped (`statSync` size check at `system-prompt.ts:35`).

## 6. Open questions / confidence gaps

- **What provider-side prompt cache is being relied on?** The "cache-stable" design implies a prefix-cache contract with whatever provider serves the request. For Workers AI and `kimi-k2.6` the actual cache mechanics are not visible in this repo. Confidence high that the *intent* is prefix stability; uncertain how much benefit each provider delivers in practice.
- **Are LSP/MCP tools included in `apiMessages` deterministically?** `messagesRef.current[1]` is rebuilt on mode/model change (`src/app.tsx:642-666`) using `[...ALL_TOOLS, ...mcpToolsRef.current, ...lspToolsRef.current]`, but if MCP initialization races with the first user prompt, the prompt sent could have a tools list mismatched with what `toolDefs` advertises that turn. Worth tracing but I didn't find a concrete bug.
- **`compactMessagesViaArtifacts` may discard the `[recalled artifacts]` system message** when `groupIntoTurns` walks the prefix (`artifact-compaction.ts:68-99`). The prefix is preserved, but if a recall block was inserted *between* turns rather than into the prefix, the grouper would treat it as orphaned. Did not verify with a test case.
- **Behaviour when `keepLastImageTurns < 0`** is "return messages unchanged" (`messages.ts:103-104`); behaviour at `0` strips every image. No tests confirm the loop's wiring at `0`.
- **The drift `onKimiMdStale` callback is invoked from inside a fire-and-forget IIFE** wrapped in try/catch (`loop.ts:1026-1037`). If extractor LLM latency exceeds the user's next turn, drift events accumulate against the *future* turn index — see comment at `loop.ts:984-988`. Confidence moderate that the sliding window does the right thing; high that the author was aware of the race.

---

## Highest-confidence findings (TL;DR)

1. **One primary loop** — `runAgentTurn` in `src/agent/loop.ts:255-1151`, wrapped by `TurnSupervisor` for the TUI. SDK and `/init` share the same function with different callbacks.
2. **Dual system messages, byte-stable index 0** — `src/agent/system-prompt.ts:46-142` and `src/ui/app-helpers.ts:289-304`. There are **no explicit prompt-cache breakpoints**; the design relies on provider prefix caches and the AI Gateway response cache.
3. **Two-tier output reduction** — `src/tools/reducer.ts` per-tool caps, plus a 10 000-char hard cap in `src/agent/loop.ts:227,931-943`, with full bytes recoverable via `expand_artifact` against `ToolArtifactStore`.
4. **Two compaction strategies** — `compactMessagesViaArtifacts` (`src/agent/artifact-compaction.ts:281`) and `summarizeMessagesViaLlm` (`src/agent/llm-summarize.ts:44`), both triggered by `shouldCompact` at 80k tokens or 12 turns, both wired into `/compact` and into `onIterationEnd`.
5. **Cross-session memory is SQLite + embeddings** in `src/memory/`. Recall fires automatically at session start (`src/ui/run-startup-tasks.ts:135`) and after auto-compaction; cross-session writes happen via `memory_remember` and via automatic extractors (`src/memory/extractors.ts`) on every tool result.
6. **No real tokenizer** — `chars / 3.5` heuristic in `src/agent/artifact-compaction.ts:36-64` is the only token count outside the model's own `usage` events.
7. **No subagents.** Code Mode (`src/code-mode/sandbox.ts`) is a tool-execution sandbox, not a separate conversation.

Report written to `/Users/season/Personal/nav/research/kimiflare-context-management.md`.
