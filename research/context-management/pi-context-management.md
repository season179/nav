# Pi — Context Management

Source tree: `/Users/season/Personal/pi` (monorepo `earendil-works/pi-mono`).
Primary packages inspected:
- `packages/agent` (`@earendil-works/pi-agent-core`): agent runtime / loop
- `packages/coding-agent` (`@earendil-works/pi-coding-agent`): the CLI's harness, session, compaction
- `packages/ai` (`@earendil-works/pi-ai`): provider transport (Anthropic / OpenAI / etc.)

## 1. Executive summary

Pi is layered: a *general agent loop* (`packages/agent/src/agent-loop.ts`) drives turns and tool calls in terms of an abstract `AgentMessage` transcript; a *coding-agent session* (`packages/coding-agent/src/core/agent-session.ts`) wires that loop up with a Claude-Code-style system prompt, a persistent JSONL session tree, threshold/overflow-driven LLM-based compaction, and an extension event bus.

The transcript is the single source of truth: every turn rebuilds the LLM context by walking the session-tree branch, materializing it into `AgentMessage[]`, optionally running `transformContext` for extensions, and finally `convertToLlm` to drop custom UI-only types before the provider call (`packages/agent/src/agent-loop.ts:282-289`). There is no separate working-set or rolling buffer.

Compaction is purely LLM-based: when `assistant.usage` (or a chars/4 estimate) crosses `contextWindow − reserveTokens`, pi walks back to find a valid cut point that keeps ~`keepRecentTokens` recent tokens, asks the model to produce a structured Markdown checkpoint of everything before that point, and records a `compaction` entry in the session JSONL. `buildSessionContext` then replays that entry as a synthetic user message at the head of the next request (`packages/coding-agent/src/core/compaction/compaction.ts:454-485`, `packages/coding-agent/src/core/session-manager.ts:390-419`). Compaction is iterative — a later compaction is fed the previous summary plus the new tail.

Cross-session memory is two things: (a) `AGENTS.md`/`CLAUDE.md` files walked up from cwd plus the agent dir, inlined into the system prompt (`packages/coding-agent/src/core/resource-loader.ts:57-113`); (b) the JSONL session log itself under `~/.pi/agent/sessions/<encoded-cwd>/…jsonl`, resumable via `--continue` / `--resume` / `--session` / `--fork`.

There is no built-in subagent / fork-loop primitive. The docs state this explicitly: pi "intentionally does not include built-in MCP, sub-agents, permission popups, plan mode, to-dos, or background bash" (`packages/coding-agent/docs/usage.md:277`). Extensions can synthesize one via custom tools, but the harness ships none.

Tokenization is a chars/4 heuristic (`packages/coding-agent/src/core/compaction/compaction.ts:232-289`); real per-turn token usage comes from the provider's `Usage` block on each assistant message.

## 2. End-to-end turn trace

A turn is a single LLM call plus its tool batch. Below traces "user types text → assistant streams → tools run → next turn" through the codebase.

### 2.1 CLI entry → AgentSession construction

`main()` parses args, builds a `SessionManager` (`create` / `open` / `continueRecent` / `forkFrom` — `packages/coding-agent/src/core/session-manager.ts:1311-1402`), constructs a `DefaultResourceLoader` to load skills/AGENTS.md/extensions, then calls `createAgentSessionFromServices` which delegates to `createAgentSession` in `packages/coding-agent/src/core/sdk.ts:202-422`.

`createAgentSession` instantiates `new Agent({…})` with these hooks (`packages/coding-agent/src/core/sdk.ts:329-385`):

```ts
agent = new Agent({
  initialState: { systemPrompt: "", model, thinkingLevel, tools: [] },
  convertToLlm: convertToLlmWithBlockImages,
  streamFn: async (model, context, options) => { … streamSimple(…) … },
  onPayload: …,                 // extension before_provider_request hook
  onResponse: …,                // extension after_provider_response hook
  sessionId: sessionManager.getSessionId(),
  transformContext: async (messages) => extensionRunnerRef.current?.emitContext(messages) ?? messages,
  steeringMode: settingsManager.getSteeringMode(),
  followUpMode: settingsManager.getFollowUpMode(),
  …
});
```

After construction, if the JSONL has prior content, the materialized transcript is hydrated back into agent state: `agent.state.messages = existingSession.messages` (`packages/coding-agent/src/core/sdk.ts:387-399`).

The resulting `AgentSession` (`packages/coding-agent/src/core/agent-session.ts:252-343`) subscribes to agent events for persistence and installs `beforeToolCall` / `afterToolCall` hooks that delegate to extensions.

### 2.2 `session.prompt(text)` → user message + system prompt

`AgentSession.prompt(text, …)` (`packages/coding-agent/src/core/agent-session.ts:962-1112`):

1. If `text` starts with `/`, try extension command dispatch (returns early if handled).
2. Emit `input` extension event (may rewrite text/images or short-circuit).
3. Expand `/skill:name args` into an inline `<skill name="…" location="…">…</skill>` block (`agent-session.ts:1148-1172`).
4. Expand file-based prompt templates.
5. If `isStreaming`, route to `agent.steer()` or `agent.followUp()` instead.
6. Validate model + auth.
7. Run a *pre-prompt* compaction check that catches sessions where the previous turn ended in an aborted/error state with stale usage (`agent-session.ts:1041-1052`).
8. Build the user message (text + optional `ImageContent`), append any `_pendingNextTurnMessages` extension asides.
9. Emit `before_agent_start` extension event; if extensions returned a `systemPrompt` override, install it; otherwise reset to `_baseSystemPrompt` (`agent-session.ts:1075-1100`).
10. Call `_runAgentPrompt(messages)` which calls `agent.prompt(messages)`.

### 2.3 `Agent.prompt` → loop entry

`Agent.prompt` (`packages/agent/src/agent.ts:325-335`) refuses overlapping runs (callers must use steer/followUp instead), normalizes the input, then `runPromptMessages` calls `runAgentLoop`:

```ts
await runAgentLoop(
  messages,
  this.createContextSnapshot(),         // { systemPrompt, messages: slice, tools: slice }
  this.createLoopConfig(options),
  (event) => this.processEvents(event), // mutates _state and fans out to listeners
  signal,
  this.streamFn,
);
```

The context snapshot is **a shallow copy of agent state at the moment of submission** (`packages/agent/src/agent.ts:414-420`). `runAgentLoop` (`packages/agent/src/agent-loop.ts:95-118`) appends prompts to `currentContext.messages`, emits `agent_start` + `turn_start` + a `message_start`/`message_end` pair per prompt, and enters `runLoop`.

### 2.4 Per-turn request assembly (`streamAssistantResponse`)

`packages/agent/src/agent-loop.ts:275-308`:

```ts
let messages = context.messages;
if (config.transformContext) {
  messages = await config.transformContext(messages, signal);
}
const llmMessages = await config.convertToLlm(messages);
const llmContext: Context = {
  systemPrompt: context.systemPrompt,
  messages: llmMessages,
  tools: context.tools,
};
const resolvedApiKey =
  (config.getApiKey ? await config.getApiKey(config.model.provider) : undefined) || config.apiKey;
const response = await streamFunction(config.model, llmContext, { ...config, apiKey: resolvedApiKey, signal });
```

What gets sent each turn:

- **System prompt** — `context.systemPrompt`, copied from `agent.state.systemPrompt` at snapshot time. Built once by `_rebuildSystemPrompt` (`packages/coding-agent/src/core/agent-session.ts:878-912`) → `buildSystemPrompt` (`packages/coding-agent/src/core/system-prompt.ts:28-175`). Layout:
  ```
  You are an expert coding assistant operating inside pi, a coding agent harness. …
  Available tools:
  - read: …
  - bash: …
  In addition to the tools above, you may have access to other custom tools…
  Guidelines:
  - …
  Pi documentation (read only when the user asks about pi itself, …
  <project_context>
    <project_instructions path="…/AGENTS.md">…</project_instructions>
  </project_context>
  <available_skills>
    <skill>…<name>…</name><description>…</description><location>…</location></skill>
  </available_skills>
  Current date: YYYY-MM-DD
  Current working directory: …
  ```
  (`system-prompt.ts:132-174`). Extensions can replace it per-turn via `before_agent_start` (`agent-session.ts:1095-1100`).

- **Tools** — `context.tools` is `agent.state.tools`, populated by `setActiveToolsByName` (`agent-session.ts:783-798`). All registered tools, with their typebox schema, are sent on every request.

- **Messages** — full transcript walk, in branch order. `convertToLlm` (`packages/coding-agent/src/core/messages.ts:148-195`) projects pi's superset of `AgentMessage` (which adds `bashExecution`, `custom`, `branchSummary`, `compactionSummary`) to the LLM-native `user|assistant|toolResult`:
  - `bashExecution` → `user` text "Ran `<cmd>`\n```…```" (`messages.ts:82-98, 152-161`); skipped when `excludeFromContext` is set (the `!!` prefix).
  - `custom` (extension asides) → `user` message (`messages.ts:162-169`).
  - `branchSummary` / `compactionSummary` → wrapped in `<summary>…</summary>` and emitted as `user` text (`messages.ts:170-183`, with prefixes/suffixes at `messages.ts:11-24`).
  - Standard `user`/`assistant`/`toolResult` pass through.

  In the coding agent, `convertToLlmWithBlockImages` additionally strips `ImageContent` when `blockImages` is set, replacing it with text and deduping (`sdk.ts:291-325`).

- **Injected context per-turn**: any extension `transformContext` handler runs at `agent-loop.ts:284-285`. In the coding agent that delegates to `extensionRunnerRef.current?.emitContext(messages)` (`sdk.ts:375-379`). Extensions can therefore inject/remove messages at *every* turn without mutating session state.

- **Prompt-cache breakpoints** — applied inside the provider transport, not by the loop. For Anthropic (`packages/ai/src/providers/anthropic.ts:889-1190`):
  - System prompt block gets `cache_control: { type: "ephemeral", ttl? }` (`anthropic.ts:905-928`).
  - The last tool definition in the array gets a `cache_control` breakpoint when the model supports it (`anthropic.ts:935-942, 1187-1189`).
  - The last block of the last user message gets `cache_control` (`anthropic.ts:1136-1158`) — this is the rolling "history" breakpoint that lets the prefix re-hit on the next turn.
  - Total of up to three ephemeral breakpoints per request. Retention (`5m` / `1h`) comes from `PI_CACHE_RETENTION` env / model metadata via `getCacheControl` (`anthropic.ts:57-65`).
  - OpenAI-compatible providers go through `applyAnthropicCacheControl` in `openai-completions.ts:511-683`, doing the same three placements when the compat layer is `anthropic`.

### 2.5 Streaming + tool execution

`streamAssistantResponse` consumes provider events into a `partialMessage` that is *pushed onto and mutated in place inside* `context.messages` so callers see live progress (`agent-loop.ts:313-367`). On `done`/`error`, the finalized `AssistantMessage` replaces the partial.

`runLoop` then checks for `toolCall` blocks (`agent-loop.ts:202-216`) and either dispatches sequentially (`executeToolCallsSequential`) or in parallel (`executeToolCallsParallel`) depending on `toolExecution`/per-tool override (`agent-loop.ts:380-516`). For each tool:

1. `prepareToolCall` runs `prepareArguments`, JSON-schema-validates via typebox, then `beforeToolCall` (extension `tool_call` event) — which may `block` execution (`agent-loop.ts:562-626`).
2. `executePreparedToolCall` runs `tool.execute(id, args, signal, onUpdate)` and collects streamed `tool_execution_update` events (`agent-loop.ts:628-663`).
3. `finalizeExecutedToolCall` runs `afterToolCall` (extension `tool_result` event) which may rewrite `content` / `details` / `isError` / set `terminate` (`agent-loop.ts:665-708`).
4. A `ToolResultMessage` is appended to context and emitted via `message_start` / `message_end` (`agent-loop.ts:727-742`).

The whole batch's `terminate` flag is logical-AND (`shouldTerminateToolBatch` at `agent-loop.ts:544-546`): the loop only exits early when *every* finalized tool result requests it.

After tool execution, `turn_end` is emitted, then `prepareNextTurn` may swap the in-flight context / model / thinking level (`agent-loop.ts:226-239`), `shouldStopAfterTurn` may bail before a follow-up LLM call (`agent-loop.ts:242-251`), and finally `getSteeringMessages()` drains queued steering items before the next turn (`agent-loop.ts:253`).

### 2.6 Post-turn: compaction check, persistence, retry

`AgentSession._handleAgentEvent` (`agent-session.ts:469-540`) persists every `message_end` via `sessionManager.appendMessage(event.message)` and remembers the last assistant message in `_lastAssistantMessage`.

After `agent.prompt` returns, `_runAgentPrompt` calls `_handlePostAgentRun` in a loop (`agent-session.ts:918-927`). That checks retry-on-error and then `_checkCompaction(msg)` (`agent-session.ts:1757-1846`):

- If the provider returned a context-overflow error for the *same* model and recovery hasn't already been attempted, the error message is popped from agent state and `_runAutoCompaction("overflow", true)` runs (`agent-session.ts:1794-1816`).
- Otherwise, total context tokens are computed (either from the assistant's `usage` or, for error messages, from `estimateContextTokens` of the transcript with a guard against stale pre-compaction usage). If `shouldCompact(contextTokens, contextWindow, settings)` returns true, `_runAutoCompaction("threshold", false)` runs (`agent-session.ts:1819-1844`).

`shouldCompact` (`packages/coding-agent/src/core/compaction/compaction.ts:219-222`):

```ts
export function shouldCompact(contextTokens, contextWindow, settings) {
  if (!settings.enabled) return false;
  return contextTokens > contextWindow - settings.reserveTokens;
}
```

Defaults: `reserveTokens = 16384`, `keepRecentTokens = 20000`, both read from `settings.compaction.*` (`packages/coding-agent/src/core/settings-manager.ts:676-690`; `compaction.ts:121-125`).

## 3. Subsystem findings

### 3.1 Per-turn request assembly

- **System prompt builder**: `buildSystemPrompt` (`packages/coding-agent/src/core/system-prompt.ts:28-175`). One-shot construction. The prompt is *static across a turn* except when:
  - Tools change → `setActiveToolsByName` rebuilds it (`agent-session.ts:783-798`).
  - Skills/context files reload → `extendResourcesFromExtensions` rebuilds it (`agent-session.ts:2063-2086`).
  - An extension's `before_agent_start` returns a `systemPrompt` (`agent-session.ts:1095-1100`).

- **Tool defs**: `agent.state.tools` is an array of `AgentTool` with a `typebox` parameters schema. The provider transport converts to Anthropic / OpenAI tool format. There is no per-turn tool filtering inside the loop itself.

- **Message history**: complete branch transcript every turn. The agent only filters at convert time (`convertToLlm`) — it never drops content silently. The full coding-agent transcript includes custom message types (bash exec, branch summaries, extension asides) that all become `user` messages at LLM time.

- **Injected context**: `transformContext` is called *each turn*, before convert (`agent-loop.ts:282-285`). The coding agent wires this to extensions only; the base agent has no built-in injection (no "latest file state" or "active todos" injection).

- **Prompt-cache breakpoints**: three ephemeral breakpoints on Anthropic-flavored backends — system, last tool, last user content block — plus OAuth-token shimming that prepends a `"You are Claude Code, Anthropic's official CLI for Claude."` system block when using Anthropic OAuth (`anthropic.ts:903-918`). Session affinity headers `x-opencode-session` / `x-opencode-client` are forwarded for opencode providers (`sdk.ts:129-164`). The `sessionId` plumbed into the `Agent` is the SessionManager's UUIDv7 — used by providers like opencode for cache routing.

### 3.2 Compaction

- **Trigger**: Threshold (`assistant.usage.totalTokens > contextWindow − reserveTokens`) or overflow (`isContextOverflow(assistantMessage, contextWindow)`). Both gated by `settings.compaction.enabled`. Checked at `agent_end` and again pre-prompt to catch aborted turns (`agent-session.ts:1041-1052, 1768-1846`).

- **Algorithm** (`packages/coding-agent/src/core/compaction/compaction.ts`):
  1. `prepareCompaction(pathEntries, settings)` finds the previous compaction (if any) → `boundaryStart` and prior `previousSummary` (`compaction.ts:644-668`).
  2. `findCutPoint` walks the entry list backwards from newest, summing `estimateTokens` (chars/4 heuristic, `compaction.ts:232-289`) until it accumulates ≥ `keepRecentTokens`, then snaps forward to the closest valid cut point (`compaction.ts:386-448`). Valid cut points are entries whose role is `user|assistant|bashExecution|custom|branchSummary|compactionSummary` — never a `toolResult` (`compaction.ts:299-336`). If the cut lands mid-turn (not on a `user`), `findTurnStartIndex` finds the prior user/bashExecution message that started that turn.
  3. Messages from `boundaryStart` to `historyEnd` become `messagesToSummarize`. If the cut splits a turn, the messages from turn-start to cut become a smaller `turnPrefixMessages`.
  4. `compact()` builds either one summary (`SUMMARIZATION_PROMPT`, `compaction.ts:454-485`) or two summaries in parallel (history + turn-prefix via `TURN_PREFIX_SUMMARIZATION_PROMPT`, `compaction.ts:725-738`), then concatenates them as `"<history>\n\n---\n\n**Turn Context (split turn):**\n\n<prefix>"`.
  5. If a prior summary exists, the `UPDATE_SUMMARIZATION_PROMPT` (`compaction.ts:487-524`) is used to merge it iteratively — preserving previous Goal/Constraints/Done items, moving In-Progress→Done, etc.
  6. A read/modified file list extracted from tool calls in the discarded range is appended as `<read-files>…</read-files><modified-files>…</modified-files>` (`compaction.ts:818-819`, `utils.ts:72-82`). Previous compaction entries' `details.readFiles`/`modifiedFiles` are carried forward (`compaction.ts:48-61`).
  7. `appendCompaction(summary, firstKeptEntryId, tokensBefore, details, fromHook)` adds a `compaction` entry to the JSONL session tree (`session-manager.ts:916-936`), then `agent.state.messages = sessionContext.messages` reloads from the rebuilt tree.

- **Summarization request**: uses `completeSimple` via the same `agent.streamFn` (so the same provider/model the user is on does the summary — there's no separate model). System prompt: `"You are a context summarization assistant. … ONLY output the structured summary."` (`utils.ts:168-170`). The transcript is *serialized as plain text* in a `<conversation>…</conversation>` block (`utils.ts:109-162`) — tool results are truncated to `TOOL_RESULT_MAX_CHARS = 2000` (`utils.ts:88-99`). Token budget: `maxTokens = min(0.8 * reserveTokens, model.maxTokens)` (`compaction.ts:570-573`).

- **Preserved vs dropped state**:
  - Dropped: every entry between the previous compaction (or start) and `firstKeptEntryId` — the originals stay in the JSONL file but are excluded by `buildSessionContext` (`session-manager.ts:390-419`).
  - Preserved verbatim: every entry from `firstKeptEntryId` onward (the "kept tail").
  - Preserved as summary: the structured Markdown checkpoint (`## Goal / ## Progress / ## Next Steps / …`) plus aggregated read/modified file lists.
  - The kept tail always starts at a user or assistant message — never inside a tool-result run — so the LLM sees a coherent turn boundary.

- **Summary placement**: `buildSessionContext` emits `createCompactionSummaryMessage(...)` *first*, then the kept messages, then any messages added after the compaction entry. On the wire, the compaction summary is a `user` message of the form:
  ```
  The conversation history before this point was compacted into the following summary:

  <summary>
  ## Goal
  …
  </summary>
  ```
  (`packages/coding-agent/src/core/messages.ts:11-17, 176-183`).

- **Branch summarization** (`packages/coding-agent/src/core/compaction/branch-summarization.ts`): a separate flow used when the user navigates the session tree to a different leaf. Collects entries from the old leaf back to the common ancestor with the target, summarizes them (newest-first within a budget of `contextWindow − reserveTokens`, default `16384`), and appends a `branch_summary` entry. This appears in future contexts as `"The following is a summary of a branch that this conversation came back from: <summary>…</summary>"` (`messages.ts:19-24, 170-175`).

### 3.3 Memory

- **In-session state**: `Agent._state` (`packages/agent/src/agent.ts:166-219`) is the live transcript + tools + model + thinking level + streaming flags + pending tool-call IDs. `state.messages` and `state.tools` are accessor-protected: assigning a new array copies it at the top level (`agent.ts:79-87`). The streaming `partialMessage` is mutated in place inside `state.messages` to keep listeners' views consistent.

- **Cross-session persistence — sessions on disk**: `SessionManager` writes JSONL into `~/.pi/agent/sessions/<encoded-cwd>/<timestamp>_<id>.jsonl` (`session-manager.ts:428-460`, `config.ts:516-518`). Path overridable by `PI_CODING_AGENT_SESSION_DIR` / settings.sessionDir. Each line is a typed entry: `session` header, `message`, `thinking_level_change`, `model_change`, `compaction`, `branch_summary`, `custom`, `custom_message`, `label`, `session_info` (`session-manager.ts:30-150`). The tree is reconstructed by following `parentId` from the leaf back to the root, so resuming or forking is just "pick a leaf, walk to root". Forking copies all non-header entries into a new file with `parentSession` set on the new header (`session-manager.ts:1359-1402`).

  Persistence is event-driven: every `message_end` triggers `sessionManager.appendMessage(event.message)` (`agent-session.ts:498-516`). Writes are delayed until the first `assistant` message so empty turns don't litter disk (`session-manager.ts:843-861`).

  Resume modes (CLI flags handled in `main.ts:215-286`):
  - `--continue` → most recent session in this cwd (`SessionManager.continueRecent`).
  - `--resume` → interactive picker (`selectSession`).
  - `--session <id|path>` → match by ID prefix locally, fall back to global search across all cwds.
  - `--fork <id|path>` → copy into current cwd as a new session.
  - `--no-session` → `SessionManager.inMemory()` (no disk).

- **Cross-session persistence — project memory**: walks from cwd up to root accumulating `AGENTS.md` / `CLAUDE.md` (`resource-loader.ts:57-113`), plus the same files from `~/.pi/agent/`. They are inlined into the system prompt under `<project_context><project_instructions path="…">…</project_instructions></project_context>` (`system-prompt.ts:60-67, 156-163`). There is no edit-or-rewrite mechanism; this is a one-way read.

- **Skills as on-demand memory**: `loadSkills` scans `~/.pi/agent/skills/` and `<cwd>/.pi/skills/` for `SKILL.md` (or `.md` files in those roots), parses frontmatter `name`/`description`, and adds an `<available_skills>` block listing only `name` / `description` / `location` (`skills.ts:335-361`). Bodies are not loaded until the assistant reads the file with the `read` tool — a Claude-Code-compatible "progressive disclosure" pattern. The `/skill:name args` syntax expands the skill body inline into the user message (`agent-session.ts:1148-1172`).

### 3.4 Token budgeting

- **Tokenizer**: there is no real tokenizer in the coding-agent path. `estimateTokens(message)` uses `Math.ceil(chars / 4)` across message variants (`compaction.ts:232-289`, with images counted as 1200 tokens / 4800 chars). The provider's reported `Usage` (`input + output + cacheRead + cacheWrite` or `totalTokens` when present) is the authoritative number used to decide whether to compact (`compaction.ts:135-137`). The chars/4 fallback is only used for error turns lacking usage, and for the in-compaction cut-point search.

- **Per-turn split**: pi does *not* allocate fixed budgets to (system / history / response). The provider's `maxTokens` is the per-response cap (`maxTokens` is passed through `SimpleStreamOptions`). The compaction reserve (`reserveTokens` default 16384) plays the role of "headroom" — the agent compacts when *measured* usage would leave less than that.

- **Truncation / backpressure**:
  - No active truncation of live conversation. The agent never drops messages mid-flight; it either compacts (replacing a prefix with a summary entry) or surfaces an overflow error and retries once.
  - Bash output is truncated upstream of the agent (in the bash tool); only `bashExecution.output` itself appears in messages. Tool results are truncated *only* in serialization for summarization (`TOOL_RESULT_MAX_CHARS = 2000`, `utils.ts:88-99`) — the LLM-visible tool result is not truncated when the message is sent on a normal turn.
  - Retry settings are configurable; on a provider error the agent emits `auto_retry_start` / `auto_retry_end` and re-runs through `agent.continue()` (`agent-session.ts:929-951, 2418-2436`). Context-overflow errors are explicitly *not* retried — they take the compact-and-retry path instead.

### 3.5 Subagents

Pi has **no built-in subagent / spawn primitive**.

- Searches across `packages/agent/src` and `packages/coding-agent/src` for `subagent` / `sub-agent` / `spawn.*[Aa]gent` / `childAgent` return zero hits (only test fixtures referencing an external community extension `HazAT/pi-interactive-subagents`).
- `packages/coding-agent/docs/usage.md:277` documents this as a design choice: "It intentionally does not include built-in MCP, sub-agents, permission popups, plan mode, to-dos, or background bash. You can build or install those workflows as extensions or packages…".
- `packages/coding-agent/docs/sdk.md:11` lists "Build custom tools that spawn sub-agents" as a *use case for the SDK*, not a feature of the harness.

The closest built-in idiom is `Agent` instances themselves — the SDK exposes `createAgentSession({ sessionManager: SessionManager.inMemory() })` so a tool implementation can spin up an ephemeral side-agent with its own transcript. There is no automatic context inheritance, no isolation enforcement, and no reintegration: the parent and child are independent.

## 4. Non-obvious design choices

- **Transcript-as-database, not-a-list**. The session is a tree of typed entries with `parentId` pointers (`session-manager.ts:44-150`). `agent.state.messages` is always derived from a branch walk, so branching/forking/resuming and compaction all operate on the same primitive. Compaction doesn't mutate or hide entries — it appends a `compaction` node that `buildSessionContext` *interprets* on read (`session-manager.ts:390-419`).

- **Compaction is iterative and structured.** The summary uses an explicit Markdown skeleton (`## Goal / ## Constraints & Preferences / ## Progress {Done/In Progress/Blocked} / ## Key Decisions / ## Next Steps / ## Critical Context`). A subsequent compaction is told to *merge* with the previous summary using `UPDATE_SUMMARIZATION_PROMPT` rather than re-summarize the whole history (`compaction.ts:487-524, 574-579`). The result is closer to a maintained "checkpoint document" than a one-shot squash.

- **Split-turn compaction**. When the cut point would land inside an assistant→toolResult run, pi keeps that whole turn but generates a smaller "turn prefix" summary so the kept suffix has explicit context (`compaction.ts:680-718, 725-738`). This is unusual — many harnesses snap the cut to a user-boundary and lose the run.

- **Serialization-before-summarize.** Before summarizing, the transcript is rendered as plain text inside `<conversation>…</conversation>` tags (`utils.ts:109-162`). That stops the summarizer from "continuing the conversation" and also lets pi truncate tool results aggressively only for summarization, without affecting the live request.

- **Three Anthropic cache breakpoints per request**. System, last tool, last user content — placed inside the provider layer, not the agent (`anthropic.ts:905-928, 935-942, 1136-1158, 1187-1189`). The rolling user-message breakpoint makes the long prefix re-cacheable as the conversation grows.

- **`bashExecution` is a first-class message role**. The `!cmd` prefix in the input area runs a local bash command, persists it as a `bashExecution` entry, and the model sees it as `Ran \`cmd\`\n\`\`\`output\`\`\`` on the next turn (`messages.ts:82-98, 152-161`). The `!!` prefix records the run but hides it from LLM context via `excludeFromContext`.

- **Steering vs follow-up queues are explicit**. `agent.steer()` injects mid-run between the current tool batch and the next LLM call; `agent.followUp()` only fires after the agent would otherwise stop (`agent.ts:236-292`, `agent-loop.ts:166-266`). Both queues have a `one-at-a-time` / `all` drain mode (`agent.ts:118-152`).

- **Tool execution mode is data-driven.** `parallel` is the default, but if any tool in the batch declares `executionMode: "sequential"` the *whole* batch runs sequentially (`agent-loop.ts:381-387`). Useful for the edit tool, which serializes file writes.

- **Early termination is an all-or-nothing hint.** A tool can return `terminate: true`, but the loop only honors it when every tool result in the batch agrees (`agent-loop.ts:544-546`). Mixed batches always continue.

- **Pi reuses the user's chosen model for compaction.** Compaction calls `compact(preparation, this.model, …, this.agent.streamFn)` (`agent-session.ts:1943-1958`). There is no "cheap-model fallback" — same provider/model as live conversation.

- **Session header records parent on fork.** `forkFrom` writes a new header with `parentSession: <sourcePath>` so the lineage is preserved (`session-manager.ts:1384-1400`). Combined with the per-entry id/parentId tree, forks act as branches in a tree-of-conversations.

## 5. Under-developed or risky areas

- **No real tokenizer.** Cut-point math during compaction relies on `Math.ceil(chars / 4)`, which can mispredict for tool-heavy turns with large JSON arguments / image placeholders. The risk window is small (the provider's reported usage gates compaction itself, and `estimateTokens` is conservative), but worst case the kept tail can land outside the model's context if a single tool result dwarfs `keepRecentTokens`.

- **Tool results are never trimmed for live context.** Compaction-serialization truncates to 2KB, but the regular turn ships the full result. A single multi-MB bash output (only loosely capped at the bash-tool layer) can blow the window before threshold check fires. Compaction handles this *after* the next assistant message, not before.

- **Compaction depends on a successful LLM call.** `_runAutoCompaction` calls the same provider to write the summary; if the provider is degraded the session degrades with it. There's no local fallback summarizer.

- **No per-tool input cap in the loop.** `validateToolArguments` validates schema, but not argument size. A pathological assistant could emit a multi-MB JSON tool call that lands in the next request unchanged.

- **The chars/4 path also drives `findCutPoint`.** If the model under-counts thinking blocks or tool calls relative to chars/4, the cut can land mid-budget. `keepRecentTokens = 20000` gives slack, but not unlimited.

- **The "trailing tokens" branch trusts last-usage.** `estimateContextTokens` reuses the last assistant's `usage.totalTokens` and adds `estimateTokens` only for messages after it (`compaction.ts:186-214`). Stale usage from before a compaction is filtered out (`agent-session.ts:1827-1837`), but the algorithm assumes `Usage.totalTokens` is reliable; some providers conflate cache read with input.

- **Project context files are inlined verbatim.** A repo with a 50KB `AGENTS.md` chain (cwd → parents → home) lands entirely in the system prompt every turn. There's no relevance ranking or truncation (`resource-loader.ts:57-113`).

- **No subagent / parallelism primitive.** A user wanting "code-reviewer agent" or fan-out task delegation must build it inside an extension or as a custom tool that spawns a new `AgentSession`. The harness gives no guard rails for budget, abort propagation, or result reintegration.

- **`transformContext` runs on the *entire* transcript every turn.** Cheap by default (the coding-agent's wrapper just defers to extensions), but an extension that does expensive work here will pay per turn.

- **Concurrent sessions in the same cwd.** `AGENTS.md` warns multiple pi sessions in a cwd will stomp each other on git, but the same is true at the session-file level if two sessions use the same path. `SessionManager` doesn't lock or detect concurrent writers.

## 6. Open questions / confidence gaps

- **Cache-retention default and per-model overrides** — `getCacheControl` (`anthropic.ts:57-65`) reads `cacheRetention` from options/model compat, but I didn't trace the resolution rules end-to-end (env `PI_CACHE_RETENTION`, model metadata, settings). High-confidence claim: there are three Anthropic breakpoints; lower-confidence: which retention applies under which conditions.

- **OpenAI Responses API path** — only inspected the Anthropic path in depth. OpenAI-compatible providers (`openai-completions.ts`) reuse the same three-breakpoint placement via `applyAnthropicCacheControl`, but the native Responses API (`openai-responses.ts`) was not traced for caching behavior.

- **Behavior of `prepareNextTurn` in production**. The hook exists in the loop (`agent-loop.ts:226-239`) and on `Agent` (`agent.ts:186-188`), but I didn't find a caller in the coding agent that uses it to swap context mid-run. Possibly extension-only.

- **Exact write-time atomicity of session JSONL** under crashes. `appendFileSync` is used (`session-manager.ts:855-859`), which is line-atomic on POSIX but not necessarily on Windows; I didn't dig into the Windows path.

- **Whether `bashExecution.excludeFromContext` is preserved across compaction.** It's checked in `convertToLlm` (`messages.ts:153-156`), so excluded entries never reach summarization input — but the JSONL still has them. Recompaction logic appears safe under this.

- **End-to-end behavior when the compaction LLM call returns malformed Markdown.** No parser-side validation: the string is stored as `summary` and replayed verbatim. The summarization system prompt strongly steers toward the structured layout, but there's no enforcement.

- **`packages/agent/src/harness/compaction/compaction.ts`** is a near-duplicate of the coding-agent compaction code (`compaction.ts:217-255` vs `compaction.ts:232-289`). Likely a harness/test variant; didn't read it in full.

---

### Cross-package map (for navigation)

- Agent loop: `packages/agent/src/agent-loop.ts:95-269` (entry), `:275-368` (stream), `:373-516` (tool exec).
- Agent class wrapper: `packages/agent/src/agent.ts:166-557`.
- Coding-agent session shell: `packages/coding-agent/src/core/agent-session.ts:252-3089`.
- Per-turn convert: `packages/coding-agent/src/core/messages.ts:148-195`.
- System prompt: `packages/coding-agent/src/core/system-prompt.ts:28-175`.
- Skills surface: `packages/coding-agent/src/core/skills.ts:335-361`.
- Compaction: `packages/coding-agent/src/core/compaction/compaction.ts:644-831`.
- Branch summarization: `packages/coding-agent/src/core/compaction/branch-summarization.ts:283-355`.
- Session tree: `packages/coding-agent/src/core/session-manager.ts:315-422` (context build), `:711-1402` (`SessionManager`).
- Anthropic cache breakpoints: `packages/ai/src/providers/anthropic.ts:889-1190`.
- OpenAI-compat cache breakpoints: `packages/ai/src/providers/openai-completions.ts:511-683`.
