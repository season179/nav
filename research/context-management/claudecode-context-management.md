# claude-code 2.1.88 Context Management — Research Report

## 1. Executive summary

claude-code 2.1.88 is a Node.js CLI whose source was extracted from the published npm bundle's source map (`README.md:1`). The agent loop is an async generator `query()` in `src/query.ts:219` that drives a single conversation through repeated model calls + tool execution; it is invoked by `QueryEngine.submitMessage()` (`src/QueryEngine.ts:209`) for SDK/REPL use and by `runAgent()` (`src/tools/AgentTool/runAgent.ts:748`) for subagents. There is one canonical loop — the same generator is reused for every kind of session, including subagents.

Context management is unusually deep for a CLI agent. The loop applies six layers of memory pressure relief per turn, in order: tool-result-size cap → `HISTORY_SNIP` (feature-gated) → microcompact → `CONTEXT_COLLAPSE` (feature-gated) → auto-compact → blocking-limit cutoff (`src/query.ts:365-647`). Post-stream there is additional reactive-compact + collapse-drain + max-output-tokens-recovery + stop-hook + token-budget logic (`src/query.ts:1062-1357`). The token estimator is a 4-bytes-per-token character count (`src/services/tokenEstimation.ts:203-208`), refined with the last API response's exact `usage` (`src/utils/tokens.ts:226`). Context-window detection supports 200K and 1M models; the auto-compact threshold reserves 20K for the summary call and 13K extra buffer (`src/services/compact/autoCompact.ts:30,62,72-91`). Cross-session memory is layered: CLAUDE.md (project), the `MEMORY.md` "memdir" (auto memory, ≤200 lines / ≤25KB index + per-topic files at `src/memdir/memdir.ts:34-38`), an optional team-memory sync, an experimental session-memory file, on-disk transcripts, and per-agent sidechain transcripts. Subagents are first-class but lightweight: they get their own `agentId`, `AbortController`, `readFileState`, permission mode, and tool pool, and reintegrate as a single `tool_result` block at the parent (`src/tools/AgentTool/AgentTool.tsx:1340-1374`).

The architecture leans hard on prompt-cache stability: every per-turn shape decision (cache scope, marker placement, schema caching, fork-cache sharing) is structured so byte-identical prefixes match the cache across iterations and subagents. Much of the cleverness is to avoid the cache breaks that naive context-management designs trigger.

## 2. End-to-end turn trace

Tracing one turn from a user message in the SDK/REPL path (the path subagents share):

1. **Entry: `ask()` → `QueryEngine.submitMessage()`** (`src/QueryEngine.ts:1186`, `:209`). Sets cwd, persists session if enabled, builds `wrappedCanUseTool`, snapshots app-state, resolves `mainLoopModel`, and computes `thinkingConfig` (`:238-282`).

2. **Build the system prompt + context** (`src/QueryEngine.ts:284-326`):
   - `fetchSystemPromptParts({ tools, mainLoopModel, additionalWorkingDirectories, mcpClients, customSystemPrompt })` → returns `{ defaultSystemPrompt, userContext, systemContext }`.
   - `defaultSystemPrompt` is an array of strings produced by `getSystemPrompt(tools, model, dirs, mcpClients)` (`src/constants/prompts.ts:444-577`): intro, system rules, doing-tasks, actions, using-your-tools, tone, output-efficiency, then `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` (`src/constants/prompts.ts:114`), then dynamic sections (`session_guidance`, `memory` from `loadMemoryPrompt()`, env, language, output style, MCP instructions, scratchpad, function-result-clearing, summarize-tool-results, etc.).
   - `userContext` from `getUserContext()` (`src/context.ts:155-189`): `claudeMd` (concatenated CLAUDE.md files from cwd walk + nested + global) and `currentDate`. Memoized for the session.
   - `systemContext` from `getSystemContext()` (`src/context.ts:116-150`): `gitStatus` (branch + main branch + git user + truncated `git status --short` ≤ 2K chars + last 5 commits; `src/context.ts:36-111`).

3. **Push the user message + attachments** (`src/QueryEngine.ts:430-665`, abbreviated). User input is wrapped as a `UserMessage`; attachments (queued commands, IDE selection, image/PDF, file-changes) are interleaved.

4. **Enter `query()`** (`src/QueryEngine.ts:675`, → `src/query.ts:219` → `queryLoop` at `:241`). State machine fields at `:204-217`.

5. **First iteration of `queryLoop`** (`src/query.ts:307`):
   - Start skill-discovery prefetch and memory prefetch (`startRelevantMemoryPrefetch`, `skillPrefetch?.startSkillDiscoveryPrefetch`) — both run during streaming and are consumed post-tools (`:301-335`, `:1592-1614`).
   - Slice `messagesForQuery = getMessagesAfterCompactBoundary(messages)` (`:365`, definition at `src/utils/messages.ts:4643`). This is how compact boundaries take effect: the loop sees only messages after the last boundary.
   - **Tool-result budget**: `applyToolResultBudget(messagesForQuery, contentReplacementState, …)` (`:379-394`) replaces oversized tool outputs.
   - **History snip** (gated, file absent in this build): `snipModule.snipCompactIfNeeded(...)` (`:401-410`).
   - **Microcompact**: `deps.microcompact(messagesForQuery, …)` (`:414-426`) → `microcompactMessages` in `src/services/compact/microCompact.ts:253`. Two variants:
     - **Time-based** (`maybeTimeBasedMicrocompact`, `:446-530`): if minutes since last assistant > threshold, content-clear old tool results (replace content with `'[Old tool result content cleared]'`) keeping the last N for `COMPACTABLE_TOOLS = {FileRead, shell, Grep, Glob, WebSearch, WebFetch, FileEdit, FileWrite}` (`:41-50`).
     - **Cached MC** (`cachedMicrocompactPath`, `:305-399`): does NOT mutate messages — emits an API-side `cache_edits` block (`src/services/api/claude.ts:3108-3162`) that deletes server-side tool results without invalidating the cached prefix.
   - **Context collapse** (gated, file absent): per-turn projection over a separate commit log (`:440-447`).
   - Build `fullSystemPrompt = appendSystemContext(systemPrompt, systemContext)` (`:449-451`, `src/utils/api.ts:437-446`). This concatenates `gitStatus` etc. as a trailing block.
   - **Auto-compact**: `deps.autocompact(...)` (`:454-543`) → `autoCompactIfNeeded` (`src/services/compact/autoCompact.ts:241`). Threshold check; if above, `trySessionMemoryCompaction(...)` first (if enabled), then `compactConversation(...)` (`src/services/compact/compact.ts:387`).
   - **Blocking limit**: if not compacting and `tokenCountWithEstimation > effectiveWindow - 3_000` (`MANUAL_COMPACT_BUFFER_TOKENS`, `src/services/compact/autoCompact.ts:65,131`), yield `PROMPT_TOO_LONG_ERROR_MESSAGE` and return (`src/query.ts:637-648`).
   - Build the API request and call the model (`:659-708`):
     - `messages: prependUserContext(messagesForQuery, userContext)` (`src/utils/api.ts:449-474`) — prepends a synthetic `UserMessage` with `<system-reminder>` wrapping `# {key}\n{value}` for each `userContext` entry (claudeMd, currentDate).
     - `systemPrompt: fullSystemPrompt` (array of strings).
     - `tools: toolUseContext.options.tools` — converted into API JSON schemas by `toolToAPISchema` (`src/utils/api.ts:119-266`) with session-stable cache, per-tool `strict`/`eager_input_streaming`/`defer_loading`/`cache_control` fields.
     - `taskBudget`, `effortValue`, `advisorModel`, `thinkingConfig`, `agents`/`allowedAgentTypes`, `mcpTools`, `skipCacheWrite` etc. threaded through.
   - Inside `callModel`, `buildSystemPromptBlocks` (`src/services/api/claude.ts:3213-3237`) maps each system-prompt string to `TextBlockParam`, attaching `cache_control` per block when its `cacheScope` is non-null. `splitSysPromptPrefix` (`src/utils/api.ts:321-435`) decides scopes: attribution header (null) → CLI prefix (org or null) → static blocks before boundary (global) → dynamic blocks after boundary (null) under the global-cache experiment, or 3-block org-scoped under the default.
   - `addCacheBreakpoints` (`src/services/api/claude.ts:3063-3211`) places **exactly one** message-level `cache_control` marker (on the last message, or second-to-last when `skipCacheWrite` for forks) and adds `cache_reference: tool_use_id` to every prior tool_result block when cached-MC is on.
   - Stream loop (`src/query.ts:708-863`): consumes the model's stream, withholds recoverable errors (prompt-too-long, max-output-tokens, media-size), pushes assistant messages, starts streaming tool execution (`StreamingToolExecutor`).
   - On stream end without tool calls: post-stream recovery paths (collapse drain → reactive compact → max-output-tokens escalate/retry → stop hooks → token budget continuation) (`:1062-1357`).

6. **Tool execution** (`:1380-1408`): either streaming (`streamingToolExecutor.getRemainingResults()`) or batch (`runTools`). Each tool yields a `user` message with a `tool_result` block; appended to `toolResults`.

7. **Tool-use summary** (`:1411-1482`): if not a subagent, fire a Haiku call to summarize the tool batch; resolved during next turn's streaming (`:1054-1060`).

8. **Post-tool attachments** (`:1580-1614`):
   - `getAttachmentMessages(...)` (`src/utils/attachments.ts:2937-2970`) — file-change attachments, queued commands as task-notification attachments, todo reminders, plan-mode reminders, MCP-instruction deltas, agent-listing deltas, deferred-tools deltas, date-change, etc.
   - Memory prefetch consume: `filterDuplicateMemoryAttachments(await pendingMemoryPrefetch.promise, readFileState)` (`:1599-1613`).
   - Skill-discovery prefetch consume (`:1620-1628`).

9. **Next iteration**: `state.messages = [...messagesForQuery, ...assistantMessages, ...toolResults]` and continue (`:1715-1727`). Loop ends on `!needsFollowUp` (model produced no tool_use), abort, max-turns, hook-stop, or error.

A turn that produces `tool_use` blocks therefore costs N+1 model calls (one per inner iteration of `queryLoop` plus the final assistant-only response). The compaction layers above all run on each inner iteration, not just turn boundaries.

## 3. Subsystem findings

### 3.1 Per-turn request assembly

**System prompt.** A `string[]` produced by `getSystemPrompt` (`src/constants/prompts.ts:444`). Static sections first (intro/system/doing-tasks/actions/tools/tone/efficiency), then `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` (`:114`), then dynamic sections. Memory prompt is loaded from `loadMemoryPrompt()` (`src/memdir/memdir.ts:419-507`) and slotted as the `'memory'` dynamic section (`src/constants/prompts.ts:495`). At API time, each string becomes a `TextBlockParam` with `cache_control` per its scope.

**Tool defs.** Each tool's API JSON schema goes through `toolToAPISchema` (`src/utils/api.ts:119-266`). Session-stable parts (name, description, input_schema, strict, eager_input_streaming) are cached by `toolSchemaCache` keyed on `name:JSON(inputJSONSchema)` to prevent mid-session GrowthBook flips from churning bytes (`:136-150` — quoted):

> ```
> Session-stable base schema: name, description, input_schema, strict,
> eager_input_streaming. These are computed once per session and cached to
> prevent mid-session GrowthBook flips (tengu_tool_pear, tengu_fgts) or
> tool.prompt() drift from churning the serialized tool array bytes.
> ```

Per-request overlays (`defer_loading` for tool-search, `cache_control` markers) are applied without mutating the cache (`:215-230`).

**Message history.** `query()` receives the full `messages` array; the loop's first slice operation is `getMessagesAfterCompactBoundary(messages)` (`src/query.ts:365`, `src/utils/messages.ts:4643`) — everything before the last compact boundary is invisible to the model.

**Injected context.** `prependUserContext(messages, userContext)` (`src/utils/api.ts:449-474`) inserts one synthetic `UserMessage` at the head:

> ```
> <system-reminder>
> As you answer the user's questions, you can use the following context:
> # claudeMd
> ...
> # currentDate
> Today's date is YYYY-MM-DD.
>
> IMPORTANT: this context may or may not be relevant to your tasks. You should not respond to this context unless it is highly relevant to your task.
> </system-reminder>
> ```

(Skipped in test envs and when context is empty.) `appendSystemContext(systemPrompt, systemContext)` (`src/utils/api.ts:437-446`) joins `systemContext` entries (`gitStatus`, optional `cacheBreaker`) as a trailing system-prompt block.

**Prompt-cache breakpoints.** Two layers:
- **System prompt scopes**: `splitSysPromptPrefix` (`src/utils/api.ts:321-435`) partitions blocks into up to 4 with `cacheScope` ∈ `'global' | 'org' | null`. Under the global-cache experiment with the boundary marker found, static content gets `global`. Otherwise everything is `org`. `buildSystemPromptBlocks` (`src/services/api/claude.ts:3213-3237`) translates each non-null scope to `cache_control` with `scope: 'global'` when applicable.
- **Message-level marker**: `addCacheBreakpoints` (`src/services/api/claude.ts:3063-3106`) places **exactly one** marker, at index `messages.length - 1` for the main thread or `messages.length - 2` for fire-and-forget forks (`skipCacheWrite`). The rationale is documented inline (`:3078-3088`) — multiple markers retain mycro local-attention KV pages longer than needed.
- **TTL**: `getCacheControl` (`src/services/api/claude.ts:358-374`) emits `ttl: '1h'` when `should1hCacheTTL(querySource)` matches a GrowthBook allowlist (`:393-434`), latched once per session to prevent mid-session flips.
- **cache_edits** (cached microcompact): `addCacheBreakpoints` inserts a `cache_edits` block at the last user message and `cache_reference: tool_use_id` on prior tool_results, deleting server-side tool results without invalidating the prefix (`:3108-3208`).

### 3.2 Compaction

The system has **five** compaction-like mechanisms; here's how each fires.

**(a) Tool-result budget** (`applyToolResultBudget`, called at `src/query.ts:379-394`). Per-message aggregate cap on tool result size; oversized content is replaced and the replacement is persisted for resume.

**(b) History snip** (gated behind `feature('HISTORY_SNIP')`). Imports `snipCompact.js` and `snipProjection.js` (not present in the external 2.1.88 source extract — `find ... -name "snipCompact*"` returns no matches). Behavior inferred only from call-site comments: client-side message snipping with a separate projection.

**(c) Microcompact** (`microcompactMessages`, `src/services/compact/microCompact.ts:253`). Two subpaths:
- **Time-based** (`maybeTimeBasedMicrocompact`, `:446-530`): triggers when minutes since last assistant message > config threshold. Content-clears compactable tool_results except the last N (≥1). After clearing, calls `resetMicrocompactState()` and `notifyCacheDeletion` so the cache-break detector doesn't false-positive (`:511-528`).
- **Cached** (`cachedMicrocompactPath`, `:305-399`): API-side cache editing. Does NOT mutate local messages. Builds a `cache_edits` block to delete tool results server-side and pins it for re-send. Only on main thread (`:272-286`).

Compactable tool set (`:41-50`):

> ```ts
> const COMPACTABLE_TOOLS = new Set<string>([
>   FILE_READ_TOOL_NAME,
>   ...SHELL_TOOL_NAMES,
>   GREP_TOOL_NAME, GLOB_TOOL_NAME,
>   WEB_SEARCH_TOOL_NAME, WEB_FETCH_TOOL_NAME,
>   FILE_EDIT_TOOL_NAME, FILE_WRITE_TOOL_NAME,
> ])
> ```

**(d) Context collapse** (gated behind `feature('CONTEXT_COLLAPSE')`). Files (`services/contextCollapse/`) are absent from this external build. Per call-site comments at `src/query.ts:428-447, 1085-1117` and `src/services/compact/autoCompact.ts:200-223`: it's a separate commit-log-based context-management system that owns the 90%/95% headroom problem when enabled and suppresses autocompact entirely.

**(e) Auto-compact** (`autoCompactIfNeeded`, `src/services/compact/autoCompact.ts:241`). The main full-summarization path.
- **Trigger** (`shouldAutoCompact`, `:160-239`): `tokenCountWithEstimation(messages) - snipTokensFreed ≥ getAutoCompactThreshold(model)`.
- **Threshold** (`getAutoCompactThreshold`, `:72-91`): `effectiveContextWindow - 13_000` where `effectiveContextWindow = contextWindow - max(maxOutputTokensForModel, 20_000)` (`:30-48`).
- **Circuit breaker**: stops after 3 consecutive failures (`:67-70, 257-265`).
- **Algorithm** (`compactConversation`, `src/services/compact/compact.ts:387`):
  1. Pre-hooks (`executePreCompactHooks`) and optional hook-supplied custom instructions (`:411-423`).
  2. Build compact prompt (`getCompactPrompt`, `src/services/compact/prompt.ts:293-303`): `NO_TOOLS_PREAMBLE + BASE_COMPACT_PROMPT + customInstructions + NO_TOOLS_TRAILER`. The base prompt requires a 9-section structured summary inside `<summary>` tags after a `<analysis>` scratchpad (`src/services/compact/prompt.ts:61-143`); `formatCompactSummary` strips the analysis before insertion (`:311-335`). The "All user messages" section explicitly preserves every non-tool-result user turn (`:73`).
  3. Forked-agent API call via `streamCompactSummary` (`src/services/compact/compact.ts:1136+`); shares parent's prompt cache when `tengu_compact_cache_prefix` is true (`:434-438`).
  4. On `prompt_too_long` from the compact call itself: `truncateHeadForPTLRetry` (`:243-292`) drops oldest API-round groups and retries up to 3 times.
  5. Reserve up to 20K output tokens for the summary (`MAX_OUTPUT_TOKENS_FOR_SUMMARY`, `:30`).
  6. Clear `readFileState` and `loadedNestedMemoryPaths` (`:520-522`). Build re-attachments (`createPostCompactFileAttachments` — up to 5 most-recent files, ≤5K tokens each, ≤50K total; `:1415+` and constants at `:122-130`).
  7. Re-emit deferred-tools delta, agent-listing delta, MCP-instructions delta, skill attachments, plan attachments (`:562-585`).
  8. Run `SessionStart` hooks (post-compact) (`:587-594`).
  9. Build `CompactionResult` and the new message stream via `buildPostCompactMessages` (`:330-338`): `[boundaryMarker, ...summaryMessages, ...messagesToKeep, ...attachments, ...hookResults]`.

**Preserved vs dropped**:
- Dropped: all `messages` content before the boundary, raw tool_use/tool_result pairs, image/document bytes (`stripImagesFromMessages` replaces with `[image]`/`[document]` for the compact call; `src/services/compact/compact.ts:145-200`), `readFileState`, `loadedNestedMemoryPaths`, skill-listing/discovery attachments (`stripReinjectedAttachments`, `:211+`).
- Preserved (post-compact): boundary marker with metadata, single `summaryMessages` user message containing the formatted summary, hook-injected re-attachments for recent files / plan / skills / agent listings / MCP instructions / deferred tools, optional `messagesToKeep` suffix for partial/session-memory compactions.

**Summary landing**: The boundary message is a `system` message with `subtype: 'compact_boundary'`; subsequent turns see the boundary (`getMessagesAfterCompactBoundary` keeps it because the slice starts at the boundary index, `src/utils/messages.ts:4645-4647`) and `normalizeMessagesForAPI` filters it out of the API payload but keeps the synthetic summary `UserMessage` (created with `isCompactSummary: true, isVisibleInTranscriptOnly: true`, `src/services/compact/compact.ts:614-624`). The summary becomes the new conversation prefix.

**Variants of full compaction**:
- **Manual `/compact`** — same `compactConversation` with `isAutoCompact: false`.
- **Partial compact** (`partialCompactConversation`, `src/services/compact/compact.ts:772-808`) — summarizes around a pivot index; direction `'from'` summarizes the tail (preserves prefix cache), direction `'up_to'` summarizes the prefix (different prompt that says "newer messages follow", `src/services/compact/prompt.ts:208-267`).
- **Reactive compact** (gated `REACTIVE_COMPACT`; file `services/compact/reactiveCompact.ts` not present here) — triggered when an in-flight API response yields a withheld 413 (`src/query.ts:1119-1175`). Single-shot guard `hasAttemptedReactiveCompact`.
- **Session-memory compact** (`trySessionMemoryCompaction`, `src/services/compact/sessionMemoryCompact.ts:514`) — when GrowthBook flags `tengu_session_memory` + `tengu_sm_compact` are on AND a session-memory file exists with content (`:403-432`), uses the already-extracted memory as the summary (no compact API call). Keeps a suffix slice computed by `calculateMessagesToKeepIndex` (`:324+`) that respects tool_use/tool_result and split-message-id pairs (`:188-230`). Limits: `minTokens: 10_000`, `minTextBlockMessages: 5`, `maxTokens: 40_000` (`:57-61`).

### 3.3 Memory

**In-session state.** Attached to `ToolUseContext` (`src/Tool.ts`):
- `messages: Message[]` — full history.
- `readFileState: FileStateCache` — file path → `{content, hash}` for the FileRead dedup-stub mechanism (cleared on compaction).
- `loadedNestedMemoryPaths` — to deduplicate nested-CLAUDE.md surfacing (cleared on compaction).
- `discoveredSkillNames` — turn-scoped, cleared on each `submitMessage` (`src/QueryEngine.ts:197-238`).
- `contentReplacementState` — tool-result-budget replacements, persisted via `recordContentReplacement` to enable resume (`src/query.ts:376-394`).
- `queryTracking` — `{ chainId, depth }` for analytics.

**Cross-session persistence:**

- **CLAUDE.md** — `getClaudeMds(filterInjectedMemoryFiles(await getMemoryFiles()))` (`src/context.ts:170-176`) collects CLAUDE.md from cwd walk + nested + global; concatenated and prepended via the `prependUserContext` `<system-reminder>` block.

- **Auto memory directory (memdir)** — `src/memdir/memdir.ts`:
  - Path: `getAutoMemPath()` (user-specific).
  - Entrypoint: `MEMORY.md`, with explicit budget — `MAX_ENTRYPOINT_LINES = 200`, `MAX_ENTRYPOINT_BYTES = 25_000` (`:34-38`).
  - `truncateEntrypointContent` (`:57+`) caps both lines and bytes.
  - `loadMemoryPrompt` (`:419-507`): dispatches between Kairos daily-log mode (append-only, `buildAssistantDailyLogPrompt` at `:327-370`), team-memory combined prompt (`TEAMMEM` feature, requires auto-memory), and the default `buildMemoryLines` (`:199-266`) flow. The prompt instructs the model to write one file per memory with `name/description/metadata.type` frontmatter and add `[Title](file.md) — one-line hook` lines to `MEMORY.md` (`:226-233`).
  - The "Searching past context" section (`:375-407`) provides grep templates for the memory directory and session transcripts.

- **Team memory** (feature-gated `TEAMMEM`) — `src/memdir/teamMemPaths.ts` and `teamMemPrompts.ts`. Shares a `MEMORY.md` between auto and team directories.

- **Session memory** (experimental, `tengu_session_memory`) — `src/services/SessionMemory/sessionMemory.ts`. File at `getSessionMemoryPath()`. Updated by a post-sampling hook (`extractSessionMemory`, `:272+`) when `shouldExtractMemory(messages)` returns true. Trigger uses two thresholds (`:134-181`): token-count growth since init AND tool-call count since last update (configurable via `tengu_session_memory` GrowthBook config; defaults at `src/services/SessionMemory/sessionMemoryUtils.ts`). Only fires on `repl_main_thread`, not subagents (`:278-281`).

- **Transcripts** — main session transcript persisted via `recordTranscript` (called inline in `QueryEngine.submitMessage`, `:716-732`). Path from `getTranscriptPath()`. The compact pipeline re-appends session metadata after compaction so `--resume` titles survive the 16KB tail window (`src/services/compact/compact.ts:706-711`).

- **Sidechain transcripts** (subagents) — `recordSidechainTranscript(initialMessages, agentId)` then per-message (`src/tools/AgentTool/runAgent.ts:735-737, 794-803`).

### 3.4 Token budgeting

**Tokenizer.** None — there is no real tokenizer. `roughTokenCountEstimation(content, bytesPerToken = 4)` returns `Math.round(content.length / 4)` (`src/services/tokenEstimation.ts:203-208`). JSON file types use 2 bytes per token (`:215-224`). Images/documents are hard-coded to 2000 tokens (`:400-411`).

For accurate counting:
- `countTokensViaHaikuFallback` (`:251-325`) — uses Haiku (Sonnet on Bedrock/Vertex with thinking) `messages.create({max_tokens: 1 or 2048})` and reads `usage.input_tokens + cache_creation + cache_read`.
- `countTokensWithBedrock` (`:437+`) — uses Bedrock `count_tokens` API.
- `countTokensWithAPI` / `countMessagesTokensWithAPI` (`:124, :140+`) — provider-aware dispatch.

**Per-turn split.** `tokenCountWithEstimation(messages)` (`src/utils/tokens.ts:226-261`) is the canonical context-size measurement. Algorithm:
1. Walk back to the most recent assistant message with `usage`.
2. From there, walk back further over any sibling assistant records sharing the same `message.id` (parallel-tool-call splits).
3. Return `getTokenCountFromUsage(usage) + roughTokenCountEstimationForMessages(messages.slice(i + 1))`. So the count is "exact prefix up to last API response" + "rough estimate for anything since". This composes correctly with interleaved tool_results.

**Truncation/backpressure.** Defined by `calculateTokenWarningState` (`src/services/compact/autoCompact.ts:93-145`) with buffers from `:62-65`:
- Auto-compact threshold: `effective - 13_000`.
- Warning threshold: `effective - 20_000`.
- Error threshold: `effective - 20_000`.
- Blocking limit (when auto-compact disabled): `effective - 3_000` (`MANUAL_COMPACT_BUFFER_TOKENS`).
- `effective = contextWindow - min(maxOutputTokensForModel, 20_000)` (`:33-48`).

Context window comes from `getContextWindowForModel` (`src/utils/context.ts:51-98`): default 200K, 1M when model name has `[1m]` suffix or `tengu_otk_slot_v1`-style config matches, plus model-capability override. Env override: `CLAUDE_CODE_MAX_CONTEXT_TOKENS`, `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, `CLAUDE_CODE_BLOCKING_LIMIT_OVERRIDE`.

**Output budgeting.**
- `getModelMaxOutputTokens(model)` (`src/utils/context.ts:149-210`) — per-model default + upper limit (Opus 4.6: 64K/128K; Sonnet 4.6: 32K/128K).
- `CAPPED_DEFAULT_MAX_TOKENS = 8_000` (`:24`) — actual ceiling sent to API for slot-reservation efficiency (BQ p99 = 4,911 tokens).
- `ESCALATED_MAX_TOKENS = 64_000` (`:25`) — retry budget if the 8K request hits the cap (`src/query.ts:1188-1221`).
- `MAX_OUTPUT_TOKENS_RECOVERY_LIMIT = 3` (`src/query.ts:164`) — recovery message injections beyond escalation.

**Task budget** (separate from output cap): `output_config.task_budget` is an API-side feature; `taskBudgetRemaining` is tracked client-side across compaction boundaries because the server can't see pre-compact history (`src/query.ts:282-291, 504-515, 1138-1146`). Also a client-side `TOKEN_BUDGET` continuation tracker (`createBudgetTracker`, `checkTokenBudget`, `src/query.ts:280, 1308-1355`) for the "+500k" UX.

**PTL retry on the compact call**: `truncateHeadForPTLRetry` drops oldest API-round groups one round at a time, parsing the API's `prompt_too_long` `tokenGap` to decide how many to drop (`src/services/compact/compact.ts:243-292`).

### 3.5 Subagents

Subagents exist and are first-class. `AgentTool` (`src/tools/AgentTool/AgentTool.tsx`, 228K) dispatches three flavors:
- **Sync local agent** — blocks the parent; runs via `runWithAgentContext` + `runAgent`.
- **Async local agent** — returns immediately with `async_launched`; runs in the background and reports via mailbox/notification (`:1327-1339`).
- **Remote agent** — `remote_launched` to CCR (`:1316-1326`); session URL handoff.
- **Teammate** — multi-agent spawn returning `teammate_spawned` (`:1301-1315`).

**Isolation** (`runAgent`, `src/tools/AgentTool/runAgent.ts:248`):
- Own `agentId` (`:347`).
- Own `AbortController` (`agentAbortController` at `:706`).
- Own `readFileState`: cloned from parent only when `forkContextMessages` is set; else fresh `createFileStateCacheWithSizeLimit(READ_FILE_STATE_CACHE_SIZE)` (`:375-378`).
- Own permission mode: overridable by `agentDefinition.permissionMode` (`:415-447`); set to auto-deny for async agents that can't show UI; `bubble` mode delegates prompts to the parent terminal.
- Own tool pool: precomputed by caller (`availableTools`) using the worker's own permission mode (`:292-300`).
- Own MCP servers: parent clients + agent-specific clients merged (`initializeAgentMcpServers`, `:95-218`); shared clients are memoized to reuse parent connections.
- Own `agentReadFileState`, own metadata file via `writeAgentMetadata` (`:738-742`).
- Own session hooks; cleared in `finally` (`:820-822`).
- Own todos namespace; cleared in `finally` (`:839-843`).
- Own background-bash + monitor MCP tasks; killed in `finally` (`:847-857`).

**Inheritance** (`:368-410`):
- `userContext` and `systemContext` start from `getUserContext()/getSystemContext()` (memoized session-wide). Override allowed by caller.
- For read-only agents (`Explore`, `Plan`), CLAUDE.md and gitStatus are dropped (`:390-410`), gated by `tengu_slim_subagent_claudemd`. Rationale in comments: saves ~5-15 Gtok/week.
- System prompt: `getAgentSystemPrompt` (`:906-940`) calls `agentDefinition.getSystemPrompt({ toolUseContext })` and runs it through `enhanceSystemPromptWithEnvDetails`.
- `forkContextMessages` is filtered through `filterIncompleteToolCalls` (`:866-904`) to drop assistant messages with orphaned tool_use blocks before they reach the subagent (which would 400 the API otherwise).
- `forkSubagent` path (`src/tools/AgentTool/forkSubagent.ts`) passes `useExactTools: true` so the fork inherits parent's thinkingConfig and produces byte-identical API request prefixes for prompt-cache hits (`:309-314` of `runAgent.ts`).

**Reintegration**: the subagent's final output is mapped to a `tool_result` block in `mapToolResultToToolResultBlockParam` (`src/tools/AgentTool/AgentTool.tsx:1298-1378`):
- `status: 'completed'` → tool_result content = subagent's final content, plus a trailing text block with `agentId: ... (use SendMessage with to: '${data.agentId}' to continue this agent)` + `<usage>` block (`:1363-1373`). One-shot built-ins (Explore, Plan) drop the trailer to save tokens (`:1352-1361`).
- Empty completion → `(Subagent completed but returned no output.)` marker (`:1347-1350`).
- `async_launched` → text instructing the parent to "tell the user what you launched and end your response" (`:1327-1338`).

The parent therefore receives the subagent's report as a single `tool_result`, not a stream of messages. The full sidechain transcript is persisted to disk and reachable via the `agentId` for resume / display.

## 4. Non-obvious design choices

- **`SYSTEM_PROMPT_DYNAMIC_BOUNDARY` sentinel string** (`src/constants/prompts.ts:114`) splits cacheable static text from dynamic per-session text using a literal `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` token searched at API-formatting time. This avoids re-architecting the prompt as nested objects.

- **Exactly one message-level `cache_control` marker per request** (`src/services/api/claude.ts:3078-3088`). Multi-marker placement is documented to defeat mycro's KV-page recycling logic. For fire-and-forget forks, the marker moves to second-to-last to avoid leaving the fork's tail in the KV cache.

- **Cached microcompact via `cache_edits`**: a server-side mechanism deletes specific tool_results from the cached prefix without invalidating the cache, by sending only diff blocks (`src/services/compact/microCompact.ts:305-399`, `src/services/api/claude.ts:3108-3208`). This is genuinely novel — most agent loops have to choose between cache hits and trimming.

- **Skill / memory prefetch during model streaming**: prefetches relevant memories and skill listings while the model is still generating, then consumes them post-tools (`src/query.ts:301-335, 1592-1628`). Avoids the latency of doing these synchronously.

- **Tool schema cache keyed by `name:JSON(inputJSONSchema)`** to defeat mid-session GrowthBook flips that would otherwise churn the serialized tool array bytes and break the prompt cache (`src/utils/api.ts:136-150`).

- **Session-memory compact path swaps a 5-30s compact API call for a zero-cost read of an already-distilled memory file** when the session has been writing extractions in the background (`src/services/compact/sessionMemoryCompact.ts:514+`).

- **Aggressive `NO_TOOLS_PREAMBLE` + `NO_TOOLS_TRAILER` on the compact prompt** (`src/services/compact/prompt.ts:19-26, 269-273`) because the forked-agent compact path inherits the parent's full tool set (cache-key match requirement) and Sonnet 4.6+ would otherwise attempt a tool call ~2.79% of the time, wasting the only turn.

- **PTL truncate-and-retry on the compact call itself** (`src/services/compact/compact.ts:243-292, 450-491`). Treats compaction failure as a routine condition and drops oldest API-round groups.

- **Tool-use-summary fired from Haiku during streaming of the next turn** (`src/query.ts:1411-1482, 1054-1060`) — uses streaming wait time as free latency budget for cosmetic summaries shown in mobile UI.

- **Multi-stage post-stream recovery ordering**: collapse-drain → reactive-compact → max-output-tokens-escalate → max-output-tokens-recover → stop-hooks → token-budget-continuation. Each has its own guard flag to prevent infinite loops (`hasAttemptedReactiveCompact`, `maxOutputTokensRecoveryCount`, `stopHookActive`, `transition.reason`).

- **Reset of `hasAttemptedReactiveCompact` is intentionally suppressed on stop-hook blocking-error retries** (`src/query.ts:1290-1297`) — a comment in the file documents that the prior version burned thousands of API calls in a `compact → still too long → error → stop hook blocking → compact → …` loop.

- **`maxOutputTokensRecoveryCount` injects a user message** rather than retrying the same request, with explicit instructions to resume mid-thought without apology or recap (`src/query.ts:1224-1228` — "Resume directly — no apology, no recap of what you were doing").

- **Auto-compact circuit breaker** (3 consecutive failures, `src/services/compact/autoCompact.ts:67-70`) — installed in response to a BQ finding that 1,279 sessions had 50+ consecutive failures (up to 3,272) wasting ~250K API calls/day globally.

- **Explore/Plan subagents drop CLAUDE.md and gitStatus by default** (`src/tools/AgentTool/runAgent.ts:386-410`) to save ~5-15 Gtok/week + ~1-3 Gtok/week respectively. Kill-switch flag included.

- **`prependUserContext` becomes a no-op in `NODE_ENV=test`** (`src/utils/api.ts:453-455`).

- **Permission-context inheritance in subagents respects `bypassPermissions`, `acceptEdits`, and `auto` modes** in the parent and won't override them (`src/tools/AgentTool/runAgent.ts:418-434`).

## 5. Under-developed or risky areas

- **Token estimation is 4 bytes per token across the board** with no real BPE tokenizer (`src/services/tokenEstimation.ts:203-208`). The API-based Haiku/Sonnet fallback exists but is rarely called on the hot path. Tool_use input is stringified through `jsonStringify` and counted at char/4, which under-counts dense JSON (the JSON file-type 2-byte path doesn't apply to tool inputs at `:416-422`). Auto-compact decisions ride on this estimate plus the last `usage.input_tokens` — accurate only between API responses, drift-prone with many post-response messages.

- **Many feature-gated subsystems are absent from the external build** (`HISTORY_SNIP`, `CONTEXT_COLLAPSE`, `REACTIVE_COMPACT`, `KAIROS`, `TEAMMEM`, `CACHED_MICROCOMPACT`'s `cachedMicrocompact.ts`). The call-site behavior depends on them (the recovery ordering at `src/query.ts:1062-1175` calls them by name), but the modules are tree-shaken out via `feature('X') ? require(...) : null`. External users get a simpler, less-defended setup than the call sites suggest.

- **Image/document token estimate is hardcoded to 2000** (`src/services/tokenEstimation.ts:411`). The actual API charge for a max-2000×2000 image is ~5333 tokens. The estimate is intentionally low to match `microCompact`'s `IMAGE_MAX_TOKEN_SIZE` — but this means a session with many images will hit `prompt_too_long` despite reporting "well under threshold".

- **`tokenCountWithEstimation` reads `usage` from the most-recent assistant message** — if the message is synthetic (API-error placeholder, summary) it's skipped (`src/utils/tokens.ts:7-20`). In long error-recovery chains, the function may walk back many messages and estimate everything since, which is exactly the drift-prone path.

- **CLAUDE.md is concatenated and prepended on every request via `prependUserContext`**, but its content (a single string under `claudeMd` in `userContext`) is part of the cache prefix. Changes to CLAUDE.md mid-session bust the prompt cache. The system memoizes `getUserContext()` for the session (`src/context.ts:155`), so changes only land on next session — but that also means `/memory edit` updates don't take effect until restart.

- **Session memory file extraction runs as a post-sampling hook** (`extractSessionMemory`, `src/services/SessionMemory/sessionMemory.ts:272+`) — if it fails silently or stalls, `trySessionMemoryCompaction` (`waitForSessionMemoryExtraction` at `src/services/compact/sessionMemoryCompact.ts:527`) blocks compaction with a timeout.

- **`messagesToKeep` in session-memory compact uses `calculateMessagesToKeepIndex` to avoid splitting tool_use/tool_result pairs and same-`message.id` thinking siblings** (`:188-230, 324+`). Edge cases here are noted as fall-back-to-legacy paths (`:540-543, 554-559`) — when boundaries can't be reasonably computed, the system silently falls back to a full compact. Hard to spot operationally.

- **The `query.ts` loop has 7+ continue sites** (`src/query.ts:1099-1116, 1152-1166, 1207-1220, 1231-1252, 1283-1305, 1321-1340, 1714-1727`). The header comment at `:151-163` ("The rules of thinking") explicitly warns that thinking blocks are a persistent footgun. Reasoning about correctness requires understanding all transition reasons.

- **`promptCacheSharingEnabled` for compact** defaults true; the false path is 98% cache miss and ~0.76% of fleet cache_creation (`src/services/compact/compact.ts:431-438`). Kill switch is preserved in case GrowthBook breaks.

- **Subagent reintegration concatenates the entire subagent transcript's final assistant content** as a `tool_result` block (`src/tools/AgentTool/AgentTool.tsx:1340-1373`). Long subagent runs can produce large tool_results; this is then subject to `applyToolResultBudget` in the parent.

- **The aggressive `NO_TOOLS` framing of the compact prompt** is a workaround for the cache-key-match constraint forcing the compact agent to inherit the full tool set. A model regression that bypasses the trailer reinstates the wasted-turn pattern.

- **`shouldUseGlobalCacheScope()` is feature-gated**; when off, no global cache scope is applied even where SYSTEM_PROMPT_DYNAMIC_BOUNDARY exists, falling back to org-only caching. Quietly halves prefix-share efficiency for users in the wrong cohort.

## 6. Open questions / confidence gaps

- **Are the `HISTORY_SNIP`, `CONTEXT_COLLAPSE`, `REACTIVE_COMPACT`, `KAIROS` modules ever present in any externally available build?** This extract has every call site but none of the implementation files. The behavioral claims I make about those subsystems are pulled exclusively from call-site comments and would need their source files to verify.

- **Does `cachedMicrocompact.ts` ship in any non-ant build?** The dynamic `await import('./cachedMicrocompact.js')` at `src/services/compact/microCompact.ts:62-69` would 404 at runtime in builds where the file is absent. Either the gate `feature('CACHED_MICROCOMPACT')` is reliably false externally, or the build process inlines a stub. I didn't find a stub.

- **Exact memdir search recall**: I inspected the prompt templates but did not verify whether the model actually adheres to the 200-line / 25K-byte budget in practice, or how `truncateEntrypointContent` handles partial mid-frontmatter truncation.

- **Multi-tool-use split-record handling in `tokenCountWithEstimation`** (`src/utils/tokens.ts:226+`) walks back over sibling assistant records with the same `message.id`. The comment explains the rationale; I did not validate the edge case where the response_id appears but `usage` is absent on later siblings.

- **`buildSystemPromptBlocks` consumes `SystemPrompt` (string[])** but I did not trace exactly how a subagent's `agentSystemPrompt` (an array from `enhanceSystemPromptWithEnvDetails`) interacts with the global-cache-scope partition — does it inherit the boundary marker or compute its own static/dynamic split?

- **Cross-session resume cost**: I observed transcript persistence and `messagesToKeep` retention but did not trace the resume code path (loading a session, replaying snipped/collapsed/compacted state). The `preservedSegment` / `anchorUuid` mechanism at `src/services/compact/compact.ts:349-367` clearly exists to make this work but the loader (`applyPreservedSegmentRelinks`) wasn't read.

- **Per-turn vs per-request distinction**: throughout this report "turn" refers to one inner iteration of `queryLoop` (one API call). The user-visible "turn" can be many such iterations (`turnCount` is incremented at `:1679`). The compaction layers run on the inner cadence, which means a long tool-using user request can trigger compaction mid-request.

- **Confidence on subagent `useExactTools` path**: I read the comment at `runAgent.ts:309-314` but didn't trace the `forkSubagent` call site to confirm it sets up the byte-identical prefix as documented.

- **`tengu_otk_slot_v1`** (max-output-tokens escalation) is read via `getFeatureValue_CACHED_MAY_BE_STALE` but the default is `false` (`src/query.ts:1195-1198`) so external users don't get the escalation path by default.

Cited files (line-anchored): `src/query.ts`, `src/QueryEngine.ts`, `src/context.ts`, `src/utils/api.ts`, `src/utils/tokens.ts`, `src/utils/context.ts`, `src/utils/messages.ts`, `src/utils/attachments.ts`, `src/constants/prompts.ts`, `src/services/tokenEstimation.ts`, `src/services/api/claude.ts`, `src/services/compact/{autoCompact,compact,microCompact,prompt,sessionMemoryCompact}.ts`, `src/services/SessionMemory/sessionMemory.ts`, `src/memdir/memdir.ts`, `src/tools/AgentTool/{AgentTool.tsx,runAgent.ts}`.
