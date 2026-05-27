# opencode — Context Management

Scope: `/Users/season/Personal/opencode`, branch `dev` (default per `AGENTS.md:3`).
Stack: TypeScript / Bun, Effect runtime, AI SDK v6 (`ai` package), Drizzle/SQLite.
The agent lives in `packages/opencode`. There is no Rust component for the core
agent loop — all loop logic is TS.

## 1. Executive Summary

opencode's context management is built around a **stateful, DB-backed session**
of `MessageV2` records. A single outer loop in `session/prompt.ts`
(`runLoop`) iterates assistant turns; an inner streaming pump in
`session/processor.ts` handles one `streamText` call from the AI SDK and emits
typed events that mutate parts in the SQLite store. The model request is
re-assembled from scratch every turn — there is no in-memory message-buffer
abstraction; instead every turn re-reads messages from the DB, converts them
via `MessageV2.toModelMessagesEffect` (which uses `convertToModelMessages` from
the AI SDK), and emits a fresh prompt with system prompts prepended.

Key facts established below with citations:

- **Loop driver**: `SessionPrompt.runLoop` (`session/prompt.ts:1244`) loops
  until the assistant's `finish` reason is terminal and there are no
  outstanding tool calls.
- **Compaction**: Triggered when post-step usage crosses `usable()` budget
  (`session/overflow.ts:20`) or on `ContextOverflowError`. Implemented as a
  fake assistant turn against a special `compaction` agent that produces a
  fixed Markdown template summary (`session/compaction.ts:42-77`).
- **Tail preservation**: After compaction, the most recent N turns up to a
  token budget are kept verbatim via `tail_start_id`; older turns become a
  Markdown summary visible to subsequent turns (`session/compaction.ts:245-294`).
- **Cross-session persistence**: Everything (messages, parts, summaries,
  permission state) lives in Drizzle tables (`session/session.sql.ts`) under
  the project DB. Sessions can also be forked (`Session.fork`,
  `session/session.ts:679`).
- **Tokenizer**: Naive `chars/4` only (`util/token.ts:1-7`). Real token
  accounting comes from the provider's usage object on `step-finish`.
- **Cache breakpoints**: Hard-coded "first 2 system + last 2 non-system"
  scheme (`provider/transform.ts:345-394`), plus per-provider
  `prompt_cache_key` / `promptCacheKey` set to the sessionID.
- **Subagents**: Spawned via the `task` tool as a fresh child session sharing
  the same DB and project, with a derived permission ruleset; their final
  text part is returned to the parent inside a `<task>` XML envelope.

## 2. End-to-End Turn Trace

Entry: A new user prompt arrives at `SessionPrompt.prompt`
(`session/prompt.ts:1215`).

1. **Session resolution & user-message persistence**
   - `sessions.get(input.sessionID)`, `revert.cleanup` (line 1218–1219).
   - `createUserMessage` (line 693) writes a `MessageV2.User` row and parts
     to SQLite. File references (`@path`), MCP resources, agent mentions, and
     `file:` URLs are expanded into `text`/`file`/`agent`/`subtask` parts
     (line 792–1071). Inline files are read via the `read` tool (line
     918–943) so the model sees the same content the tool would have
     produced — this is opencode's main "injected file context" mechanism.
   - Plugin hook `"chat.message"` fires (line 1073).

2. **Enter loop** (`session/prompt.ts:1232-1233`):
   `loop(...) → state.ensureRunning(...) → runLoop(sessionID)`. `runLoop`
   (line 1244) is the central agent loop. Per iteration:

3. **Reload history from DB** (line 1256):
   `MessageV2.filterCompactedEffect(sessionID)` streams every message+parts
   for the session and applies compaction reordering: if a compaction summary
   was completed, the head messages before it are dropped and the recent
   tail (anchored by `tail_start_id` on the compaction part) is spliced
   back after the summary (`message-v2.ts:1014-1064`). This is the only
   place opencode "compacts" the message list at request time — all earlier
   messages still exist in DB but are filtered out of the model request.

4. **Identify the "latest" turn state** (line 1258):
   `MessageV2.latest` (`message-v2.ts:1078`) returns
   `{ user, assistant, finished, tasks }` by walking `msgs` and picking the
   max-`id` user, max-`id` assistant, and max-`id` finished assistant.
   `tasks` are unprocessed `compaction`/`subtask` parts on user messages
   newer than the last finished assistant.

5. **Termination test** (line 1262-1291): if the most recent assistant has
   a non-`tool-calls` finish, no outstanding tool calls, and the last user
   predates it, exit the loop. Otherwise increment `step`.

6. **First-step side jobs** (line 1294-1300): on step 1, fork a `title`
   subagent (separate small model call via `llm.stream` with `system: []`
   and no tools) and a `SessionSummary.summarize` job that computes file
   diffs over snapshots. Both are `Effect.forkIn(scope)` and do not block
   the main loop.

7. **Process queued tasks first** (line 1303-1320):
   - If `tasks.pop()` is a `subtask`: `handleSubtask` (line 303) creates an
     assistant message holding a single `tool` part for the `task` tool and
     invokes it via the task tool's `promptOps` — see §3.5.
   - If `tasks.pop()` is a `compaction`: call `compaction.process(...)`,
     break the loop on "stop".

8. **Auto-compaction check** (line 1322-1329): if the previous finished
   assistant's `tokens` exceed `usable()` budget, call `compaction.create`
   to enqueue a synthetic compaction user message and `continue` —
   compaction will actually run on the next loop iteration via the task
   branch.

9. **System reminders** (line 1341):
   `SessionReminders.apply` (`session/reminders.ts:14`) appends synthetic
   user parts: `plan.txt` if agent="plan", `build-switch.txt` if switching
   plan→build, optionally `plan-mode.txt` under
   `experimentalPlanMode`. These mutate the in-memory `msgs` only (not
   the DB row).

10. **Create the assistant message row** (line 1347-1362). It is upserted to
    DB before any model call, so the streaming loop can attach parts as
    they arrive.

11. **Tool resolution** (line 1387-1401): `SessionTools.resolve`
    (`session/tools.ts:24`) builds a `Record<string, AITool>` from the
    `ToolRegistry` plus MCP tools, filtered by the agent+session permission
    ruleset (`session/llm/request.ts:188-194`). Each AI-SDK tool wraps an
    Effect that funnels execution back through the SessionProcessor for
    streaming part updates.

12. **System assembly** (line 1435-1443):
    ```ts
    const [skills, env, instructions, modelMsgs] = yield* Effect.all([
      sys.skills(agent),
      sys.environment(model),
      instruction.system(),
      MessageV2.toModelMessagesEffect(msgs, model),
    ])
    const system = [...env, ...instructions, ...(skills ? [skills] : [])]
    ```
    - `env` (`session/system.ts:48-63`): a single string declaring model ID,
      cwd, worktree, git status, platform, today's date.
    - `instructions` (`session/instruction.ts:154`): contents of global
      `AGENTS.md` + (unless disabled) `~/.claude/CLAUDE.md`, plus the
      nearest project-level `AGENTS.md`/`CLAUDE.md`/legacy `CONTEXT.md`
      (`session/instruction.ts:14-18`, `109-152`), plus any extra
      `config.instructions` globs or URLs. Each becomes a separately
      prefixed string `"Instructions from: <path>\n<body>"`.
    - `skills` (`session/system.ts:65`): if the agent's permission allows
      the `skill` tool, a verbose dump of available skills.
    - **Note**: the provider-specific base prompt (anthropic/gpt/codex/...)
      is NOT added here — it is prepended later in `LLMRequestPrep.prepare`.

13. **Last-step warning** (line 1451): if `step >= agent.steps`, append a
    final assistant message containing `max-steps.txt` after the model
    messages. This message tells the model tools are now disabled and
    forces a text-only summary turn.

14. **Inject system-reminder on follow-up user texts** (line 1415-1431):
    for any user message after the previous finished assistant, plain
    non-synthetic text parts are wrapped in a `<system-reminder>` block
    before being sent. This only mutates the in-memory copy used for
    `toModelMessagesEffect`.

15. **Hand off to streaming processor** (line 1444):
    `handle.process({ user, agent, system, messages, tools, model, ... })`
    → `processor.process` (`session/processor.ts:780`).

16. **Request preparation** in `LLM.run` (`session/llm.ts:81`):
    a. `LLMRequestPrep.prepare` (`session/llm/request.ts:54`):
       - Builds `system[]` as one or two concatenated strings: provider
         prompt (or `agent.prompt` override) + the strings from step 12 +
         `user.system` if any (line 56-65). After the plugin
         `experimental.chat.system.transform` hook, if a plugin grew the
         array, slots ≥1 are folded into a single trailing block (line
         72-76).
       - Resolves AI SDK `providerOptions`, temperature, topP, topK,
         `maxOutputTokens` (`ProviderTransform.maxOutputTokens`,
         `provider/transform.ts:OUTPUT_TOKEN_MAX`). Plugin hook
         `chat.params` can override (line 105).
       - Resolves headers, plugin hook `chat.headers` (line 125).
       - Sorts tools alphabetically (line 165) for cache stability.
       - For OpenAI OAuth, system goes to `options.instructions` instead
         of as a system role (line 90); for workflow models it is sent
         outside the message list (line 92-103).

    b. Provider/model selection happens inside `LLM.run` via
       `provider.getLanguage(input.model)` (line 95). Two runtimes:
       - `LLMNativeRuntime` (opt-in via `experimentalNativeLlm`,
         `session/llm.ts:220-258`) — uses opencode's own `@opencode-ai/llm`
         crate.
       - Default: AI SDK `streamText` (line 270-340) with
         `wrapLanguageModel` and a `transformParams` middleware that, for
         streams, runs `ProviderTransform.message` to apply provider quirks
         and prompt-cache `providerOptions`.

17. **Stream pump** (`session/processor.ts:780-849`): the AI SDK fullStream
    is converted to `LLMEvent`s via `LLMAISDK.toLLMEvents`
    (`session/llm.ts:357-364`) and drained event-by-event:
    `reasoning-*`, `text-*`, `tool-input-*`, `tool-call`, `tool-result`,
    `tool-error`, `step-start`, `step-finish`, `finish`,
    `provider-error`. Each event mutates a `MessageV2.Part` in SQLite via
    `session.updatePart`/`updatePartDelta`.

    `step-finish` records token usage from the provider, computes cost
    against `model.cost?.tiers`, attaches a snapshot patch part, and
    triggers `isOverflow` (line 610-616). If overflow, sets
    `ctx.needsCompaction = true`, and the `Stream.takeUntil` clause
    (line 794) stops the drain immediately. The processor returns
    `"compact"`.

    Doom-loop protection (line 424-449): if the last three tool parts are
    identical (same tool name + identical inputs), the processor calls
    `permission.ask({ permission: "doom_loop", ... })`.

18. **After processor returns** (`session/prompt.ts:1476-1490`):
    - `"stop"` → break the outer loop.
    - `"compact"` → `compaction.create({ auto: true, overflow: !finish })`,
      which inserts a `user` row with a single `compaction` part. Next
      iteration of `runLoop` picks it up via `tasks.pop()`.
    - `"continue"` → next iteration.

19. **Post-loop** (line 1495): `compaction.prune` runs in background to
    zero out old tool outputs above the `PRUNE_PROTECT` window, then
    `lastAssistant(sessionID)` returns to the caller.

## 3. Subsystem Findings

### 3.1 Per-Turn Request Assembly

Final `messages: ModelMessage[]` is built as

```
[system messages from system[]] (unless OpenAI OAuth or workflow)
  +
[converted UI messages from MessageV2.toModelMessagesEffect(...)]
  +
[{ role: "assistant", content: MAX_STEPS }] if step >= agent.steps
```

`MessageV2.toModelMessagesEffect` (`message-v2.ts:630-913`) does the
heavy lifting:

- Drops assistant messages with errors unless they contain non-step/reasoning
  parts (`message-v2.ts:746-754`).
- Tool parts become `tool-${name}` SDK parts; completed parts include
  truncated output (`toolOutputMaxChars` capped at 2 000 chars during
  compaction calls, `compaction.ts:37` and `compaction.ts:407-409`).
- `compaction` user parts are replaced with literal text
  `"What did we do so far?"` (`message-v2.ts:726-731`).
- Reasoning parts are passed through with `providerMetadata` only when the
  current model matches the assistant message's original model
  (`message-v2.ts:861-874`) — otherwise reasoning is downgraded to plain
  text so signed-thinking blocks don't break replay.
- For providers that can't carry media in tool results, attachments are
  extracted and re-injected as a synthetic user message
  (`message-v2.ts:744, 800-802, 878-897`).
- Step-start–only messages are filtered out before `convertToModelMessages`
  (`message-v2.ts:906`).

System prompt order, per `LLMRequestPrep.prepare`
(`session/llm/request.ts:56-76`):

1. `agent.prompt` if set; otherwise `SystemPrompt.provider(model)` —
   one of `anthropic.txt`, `gpt.txt`, `beast.txt` (for `gpt-4`/`o1`/`o3`),
   `codex.txt` (for `gpt*codex`), `gemini.txt`, `kimi.txt`, `trinity.txt`,
   or `default.txt` (`session/system.ts:19-33`).
2. `env` block (model id, cwd, worktree, git, platform, date).
3. Instruction files (global + project-level + URL fetches).
4. Skills block.
5. `user.system` (per-message system override) if present.
6. Optionally `STRUCTURED_OUTPUT_SYSTEM_PROMPT` (`session/prompt.ts:1443`).

That entire stack is concatenated to a single string and pushed as the first
element of `system[]`. Plugin hook output gets folded into a second
"trailing block" so there are at most two system messages (allowing two cache
breakpoints — see §3.4).

Tool defs: built by `SessionTools.resolve` from `ToolRegistry.tools(...)` +
MCP tools, with input schemas transformed per provider via
`ProviderTransform.schema(...)` (`session/tools.ts:80`,
`session/tools.ts:123`). The tool set is filtered against agent/session
permission rulesets in `resolveTools`
(`session/llm/request.ts:188-194`) — tools set to `deny` are dropped from
the request.

Injected context for the **current** user message goes through
`createUserMessage` (`session/prompt.ts:693-1213`):

- `@file` / `@dir` references are expanded by issuing the actual `read`
  tool inline and pasting both the synthetic tool-call message and the
  output (`session/prompt.ts:894-973`). The model sees a transcript
  identical to what a tool call would have produced.
- MCP resources read via `mcp.readResource` and inlined the same way
  (line 796-846).
- `@agent` mentions become an `agent` part plus an instruction to call the
  task tool (line 1048-1063).

### 3.2 Compaction

**Trigger** (`session/processor.ts:610-616`, `session/prompt.ts:1322-1329`):

- After every `step-finish`, `isOverflow({ tokens, model })`
  (`session/overflow.ts:20-32`) checks
  `tokens.total || input+output+cache.read+cache.write >= usable(model)`,
  where `usable` = `model.limit.input - reserved` (or
  `context - maxOutputTokens` if no input limit), and `reserved` is
  `cfg.compaction.reserved ?? min(20 000, maxOutputTokens)`
  (`session/overflow.ts:8-18`).
- If true, the processor halts the drain (`Stream.takeUntil`,
  `session/processor.ts:794`) and returns `"compact"`. `runLoop` then
  calls `compaction.create`.
- Alternative path: `ContextOverflowError` parsed from a provider response
  in `MessageV2.fromError` (`message-v2.ts:1146-1159`,
  `message-v2.ts:1175-1198`) is caught in `processor.halt`
  (`session/processor.ts:754`) and sets `needsCompaction = true` with
  `overflow: true`.

**Algorithm** (`SessionCompaction.process`,
`session/compaction.ts:344-582`):

1. Pick "head" vs "tail":
   - `select` (line 245-294) computes turns (each user message starts a
     turn). Take the last `cfg.compaction.tail_turns ?? 2`.
   - `preserveRecentBudget` (line 136-141) =
     `cfg.compaction.preserve_recent_tokens ?? min(8000, max(2000, usable*0.25))`.
   - Walk backwards through recent turns, estimating each with
     `Token.estimate(JSON.stringify(toModelMessagesEffect(...)))` (line
     237-243). Greedily keep whole turns; if a single turn won't fit,
     `splitTurn` (line 161-184) finds a midpoint where the suffix fits.
   - Returns `{ head, tail_start_id }`. The compaction `Part` row stores
     `tail_start_id` so the same boundary survives replay.

2. Run an actual LLM call against the `compaction` agent
   (`session/compaction.ts:383-457`):
   - Strip media (`stripMedia: true`) and truncate tool outputs to 2 000
     chars when converting to model messages (line 406-409).
   - Final user message holds the prompt: either `previousSummary`-aware
     "Update the anchored summary..." or "Create a new anchored
     summary..." (line 123-134), followed by the literal Markdown
     template (line 42-77) with sections `## Goal`, `## Constraints &
     Preferences`, `## Progress`, `## Key Decisions`, `## Next Steps`,
     `## Critical Context`, `## Relevant Files`.
   - The summary streams in as a normal assistant message with
     `summary: true` and `mode: "compaction"` (line 411-437). It is
     stored in the same `message`/`part` tables.

3. If overflow led to `replay`, the original triggering user message is
   re-issued after compaction (line 477-504) — opencode replays the
   user's last turn against the compacted context.

4. Plugin hook `experimental.compaction.autocontinue` decides whether to
   inject a synthetic "Continue if you have next steps..." user message
   (line 506-558). Marker `compaction_continue: true` in the part metadata.

5. Returns `"continue"` (success) or `"stop"` (the compaction itself
   overflowed). On `"stop"`, the assistant carries a
   `ContextOverflowError` and the loop terminates (line 459-468).

**Preserved vs dropped state**: messages before `tail_start_id` are not
deleted from DB; `filterCompacted` (`message-v2.ts:1014-1064`) just skips
them on subsequent reloads and reorders so the runtime sees
`[compaction-user, summary, ...tail..., later-user-and-on]`. Older tool
outputs may additionally have their text replaced with
`"[Old tool result content cleared]"` (`message-v2.ts:791-794`) when
`time.compacted` is set by `prune`.

**Where the summary lands**: in the same `MessageTable` as a regular
assistant message — there is no separate "summary store". The
`compaction` user part anchors the boundary and points at `tail_start_id`.

### 3.3 Memory

**In-session**:

- `MessageV2.WithParts[]` is re-read from SQLite at the top of every
  `runLoop` iteration (`session/prompt.ts:1256`). There is no in-memory
  buffer of past messages held across iterations. The "running"
  `ProcessorContext` (`session/processor.ts:73-82`) tracks only the
  *current* turn's in-flight reasoning/text/tool parts.
- Per-instance scratch lives in `InstanceState`s. The notable one is
  `Instruction`'s `claims` map (`session/instruction.ts:69-76`,
  `178-220`), which tracks which AGENTS.md / CLAUDE.md files have already
  been attached for a given assistant message so the same one isn't
  re-attached when several files are read in the same turn.

**Cross-session**:

- Everything is in Drizzle tables (`session/session.sql.ts`), backed by
  the project's SQLite DB. Sessions can be listed, forked
  (`Session.fork`, `session/session.ts:679-719`), removed, etc. Forking
  copies messages and parts up to an optional `messageID`, remapping
  IDs and rewriting compaction `tail_start_id` accordingly.
- Sessions also carry cumulative `tokens`, `cost`, optional `summary`
  (file-diff totals), share URL, archive timestamp, and `revert` state
  (`session/session.ts:208-227`).
- There is no separate "long-term memory" / vector store / project-wide
  notes file written by the agent loop. The closest things are:
  - AGENTS.md / CLAUDE.md files read on every turn
    (`session/instruction.ts:154-168`).
  - The `plan` Markdown file emitted by Plan-mode under
    `.opencode/plans/<ts>-<slug>.md` (`session/session.ts:371-376`,
    `session/reminders.ts:71-87`).
  - Snapshots & file diffs stored separately by `Snapshot`/`SessionSummary`.

### 3.4 Token Budgeting

- **Tokenizer**: `util/token.ts` is `Math.round(input.length / 4)`. There
  is no `tiktoken`/`gpt-tokenizer` dependency anywhere under
  `packages/opencode/src/` (verified by grep). The 4-chars-per-token
  estimate is used only for compaction budgeting decisions
  (`session/compaction.ts:237-243`, `298-342`).
- Authoritative token counts come from the provider via the AI SDK's
  `Usage` object, captured in `step-finish`
  (`session/processor.ts:558-562, 577-587`) and turned into
  `Session.getUsage(...)` (`session/session.ts:378-443`) which separates
  `input`, `output`, `reasoning`, `cache.read`, `cache.write`, applies the
  pricing model, and updates the assistant message + session row.
- **Per-turn split / truncation**:
  - System: hard-folded to ≤ 2 entries (`session/llm/request.ts:72-76`).
  - Tool outputs: truncated only during compaction-time conversion
    (`toolOutputMaxChars: 2_000`, `session/compaction.ts:37, 406-409`).
    During normal turns there is no per-tool truncation at the
    `toModelMessages` layer — each tool's own `execute` is responsible
    (e.g. `Truncate` service in `session/tools.ts:177`).
  - "Recent budget" for tail selection: `min(8000, max(2000, 0.25 * usable))`
    overridable via `cfg.compaction.preserve_recent_tokens`
    (`session/compaction.ts:136-141`).
  - `MAX_STEPS` enforcement: `agent.steps ?? Infinity`
    (`session/prompt.ts:1339-1340`). On the last step, the request
    appends an assistant message containing `max-steps.txt`
    (line 1451) telling the model tools are disabled.
- **Cache breakpoints** (`provider/transform.ts:345-394`):

  ```ts
  const system = msgs.filter((m) => m.role === "system").slice(0, 2)
  const final  = msgs.filter((m) => m.role !== "system").slice(-2)
  // attach cacheControl: { type: "ephemeral" } to those 4
  ```

  applied only to Anthropic/Vertex-Anthropic/Bedrock/OpenRouter/etc.
  (line 437-449). For Anthropic and Bedrock, the marker is set at the
  message-level `providerOptions`; otherwise it is set on the last content
  block (line 370-391). Per-session `prompt_cache_key`
  /`promptCacheKey`/`x-session-affinity` headers are added in
  `ProviderTransform.options` (`provider/transform.ts:1158-1175`) and
  in request headers (`session/llm/request.ts:168-183`).

- **Backpressure**: there is no explicit backpressure on the AI SDK
  stream. `Stream.takeUntil(() => ctx.needsCompaction)` is the only
  short-circuit (`session/processor.ts:794`); otherwise events drain
  as fast as the provider streams them.

### 3.5 Subagents

opencode has *two* kinds of "non-primary" workers; only one is a true
agent subprocess:

**Subagents via the `task` tool** (`tool/task.ts`):
- A subagent is a normal `Agent.Info` with `mode: "subagent"`
  (`agent/agent.ts:32`).
- `TaskTool.execute` (`tool/task.ts:106-291`) creates a *new* `Session`
  with `parentID = currentSessionID`. Permission ruleset is derived via
  `deriveSubagentSessionPermission` (`agent/subagent-permissions.ts:17`):
  parent agent's edit denies + parent session's external_directory rules
  and denies + default `todowrite`/`task` denies unless the subagent's
  own ruleset enables them.
- The new session calls back into `SessionPrompt.prompt` via the
  `promptOps` closure passed through `ctx.extra`
  (`session/prompt.ts:132-138`, `tool/task.ts:180-200`). This means the
  subagent runs the exact same `runLoop` against its own DB-backed
  history.
- **Isolation**: separate session row, separate message rows, separate
  `permission`. Same project, same DB, same instance scope.
- **Inheritance**: model defaults to the parent assistant's model if the
  subagent doesn't declare one (`tool/task.ts:164-167`); permission as
  above; the subagent does **not** see parent messages — it gets only
  the `prompt` string the parent provided.
- **Reintegration**: when foreground, the final `text` part of the
  subagent's run is wrapped in `<task id="..."><task_result>...</task_result></task>`
  (`tool/task.ts:54-56, 273-279`) and returned as the tool output. When
  `background: true` (gated by `experimentalBackgroundSubagents`,
  `tool/task.ts:111-115, 233-258`), the parent immediately gets a
  `<task state="running">` placeholder and is notified later by a
  synthetic user message injected via `ops.prompt(...)`
  (`tool/task.ts:203-226`).
- **Title subagent** at `session/prompt.ts:241-301` is a *non*-loop call:
  it uses `LLM.stream` directly with no tools and an empty system to
  generate a session title from the first user message.

**Synthetic "compaction agent"**:
- A built-in `Agent` named `compaction` (`agent/agent.ts` imports
  `prompt/compaction.txt`) runs in a `SessionProcessor` *inside the same
  session*, producing an assistant message marked `summary: true,
  mode: "compaction"`. Strictly speaking this is not a subagent — same
  session, same loop entry point — but it functions as an in-band worker.

## 4. Non-Obvious Design Choices

- **Re-read everything every turn.** Rather than keeping a long-lived
  `ModelMessage[]` in memory, `runLoop` re-streams parts from SQLite
  every iteration and re-runs `filterCompacted` and `toModelMessages`.
  This keeps the "source of truth" in one place but pays a JSON+SQL
  cost on every turn.

- **Tool calls disguised as model output for file injection.** When a
  user writes `@path/to/file.ts`, the loop actually *executes* the
  `read` tool inline at user-message-construction time and embeds the
  resulting synthetic "Called the Read tool with..." text plus output
  into the user message (`session/prompt.ts:894-943`). The model sees a
  prior tool transcript, not a raw file blob.

- **Compaction is itself an LLM turn, not an algorithmic summary.**
  `SessionCompaction.process` reuses the full `SessionProcessor` plumbing
  to run a regular streaming assistant turn with a fixed template
  prompt. Token counting, retries, and error handling are all the same
  code path as a normal turn — and a compaction call can itself overflow,
  in which case the session terminates with a `ContextOverflowError`
  (`session/compaction.ts:459-468`).

- **Doom-loop guard via permissions.** The "model repeated the same tool
  call 3 times" check (`session/processor.ts:425-449`) is implemented as
  a `permission.ask({ permission: "doom_loop", ... })` — the same
  permission machinery used to ask the user about destructive
  operations. Users can configure it to auto-deny / auto-allow per
  agent.

- **System reminders inserted at the user-message layer, not as system.**
  After step 1, every subsequent plain user text gets wrapped in
  `<system-reminder>...</system-reminder>` before being sent
  (`session/prompt.ts:1415-1431`). This survives prompt-cache hits on the
  system block.

- **Two-system-message ceiling for cache stability.** Plugin output is
  folded into a second slot rather than appended as a third
  (`session/llm/request.ts:72-76`) so Anthropic's 4-cache-block budget
  isn't blown on system content alone.

- **Per-step snapshot/diff tracking.** A filesystem snapshot is taken
  *before* the stream starts (`session/processor.ts:109`) so that even
  AI-SDK-internal tool calls that fire before `step-start` get an
  origin point. Every `step-finish` re-snapshots and emits a `patch`
  part with the file diff.

- **`prune` is separate from compaction.** `SessionCompaction.prune`
  (`session/compaction.ts:298-342`) walks backwards over recent
  messages, leaves the last `PRUNE_PROTECT = 40_000` tokens of tool
  output intact, then marks older tool outputs `time.compacted = now`.
  At conversion time these become `"[Old tool result content cleared]"`
  (`message-v2.ts:791-792`). It runs after the main loop ends
  (`session/prompt.ts:1495`) as a fire-and-forget effect.

- **Tools are alphabetically sorted before sending.**
  `session/llm/request.ts:165` — a small but real prompt-cache hit
  optimization.

- **`promptOps` injection.** `SessionPrompt` creates a closure of itself
  and threads it into the `task` tool via `ctx.extra.promptOps`
  (`session/prompt.ts:132-138`, `tool/task.ts:180`). This avoids
  importing `SessionPrompt` from the tool, which would create a
  service-cycle in the Effect layer graph.

## 5. Under-developed or Risky Areas

- **Char/4 tokenizer.** Compaction's tail-budget decision relies on
  `length/4` (`util/token.ts:3-5`, used in `session/compaction.ts:237-243`).
  For non-Latin scripts, code-heavy messages, or huge JSON tool outputs,
  this estimate drifts substantially. The actual overflow check uses the
  provider's usage numbers, but the `tail_turns` selection that decides
  *what to keep* across compaction does not.

- **Compaction-of-compaction overflow path.** If the compacted history
  *still* exceeds the model's context, `SessionCompaction.process`
  returns `"stop"` and the assistant gets a `ContextOverflowError` —
  there is no second-pass strategy (e.g. drop more turns, switch
  models, lossy-truncate). See `session/compaction.ts:459-468`.

- **Naive `filterCompacted` boundary handling.** `filterCompacted`
  (`message-v2.ts:1014-1064`) is non-trivial: it relies on consistent
  `parentID` linking and on `tail_start_id` pointing at a still-present
  message. Forks remap these via `Session.fork`
  (`session/session.ts:712-714`), but if a tail-anchor message is
  removed by `Session.removeMessage` mid-fork, the next turn's reorder
  could either keep the entire pre-compaction tail or drop it — there
  is no consistency check.

- **No real backpressure or rate-limiting in the stream path.** Token
  budget is a *post-step* check; a runaway 1M-token tool output streamed
  in a single step would be appended to history fully before any
  compaction decision can fire.

- **Instruction file reads on every turn.** `Instruction.system()`
  (`session/instruction.ts:154-168`) reads global + project AGENTS.md /
  CLAUDE.md from disk each turn at concurrency 8, plus fetches any
  configured URLs (with a 5s timeout, no caching beyond that:
  `session/instruction.ts:94-102`). For URL-backed instructions this
  is a per-turn HTTP fetch.

- **Subagent message inheritance is implicit prompt-only.** A subagent
  has no access to the parent session's reasoning or tool history beyond
  what the parent textually included in the `prompt` argument. That's
  deliberate isolation, but documentation does not flag it; a user who
  expects the subagent to "see what just happened" will hit silent
  context loss.

- **`title` subagent retries.** Configured with `retries: 2`
  (`session/prompt.ts:282`) for a small model, but no error event is
  surfaced beyond a log warning — a stuck title is invisible.

- **Background subagents are still feature-flagged.** Gated on
  `OPENCODE_EXPERIMENTAL_BACKGROUND_SUBAGENTS=true` (`tool/task.ts:112-115`).

## 6. Open Questions / Confidence Gaps

- **Native LLM runtime.** I traced `LLMNativeRuntime.stream`'s caller
  path (`session/llm.ts:220-258`) but did not open
  `session/llm/native-runtime.ts` or `native-request.ts`. The general
  shape (LLMEvent stream over a WebSocket/HTTP executor) matches the AI
  SDK path's interface, but whether prompt-caching and tool execution
  are handled identically there is unverified. Medium confidence.

- **Plugin trust boundary.** Plugins can mutate `system[]`
  (`session/llm/request.ts:67-76`) and `messages` (via
  `experimental.chat.messages.transform`, `session/prompt.ts:1433`).
  These transforms run inside the same Effect scope with no obvious
  sandbox; I did not audit whether plugin code can read the auth info
  it's transforming. Low confidence on security implications.

- **Cross-instance behavior.** `InstanceState` keys most services by
  directory (`packages/opencode/AGENTS.md:104-113`), but the DB itself
  is project-scoped. Whether two opencode TUIs in different worktrees
  of the same project share the same compaction state was not directly
  verified.

- **Skill loading cost.** `sys.skills(agent)` (`session/system.ts:65-77`)
  emits a "verbose" dump of every available skill on every turn. The
  size of that dump for a project with many skills, and whether it
  benefits from the cache breakpoint, was not measured.

- **Reasoning preservation across model swaps.** The
  `differentModel` branch in `toModelMessages`
  (`message-v2.ts:861-874`) downgrades reasoning to text when the
  current model differs from the message's original. The exact
  comparison is `${providerID}/${id}` (`message-v2.ts:743`), which
  would also trigger on minor version swaps within the same provider
  — possibly more aggressive downgrading than intended.

- **Configurability surface.** I cited config knobs
  (`cfg.compaction.tail_turns`, `preserve_recent_tokens`, `reserved`,
  `prune`, `cfg.experimental.continue_loop_on_deny`,
  `cfg.experimental.primary_tools`, `cfg.shell`) but did not open
  the schema definitions to confirm defaults beyond what the consuming
  code reveals. Medium confidence on the defaults.

---

**Report location**: `research/opencode-context-management.md` (this file).

**Highest-confidence findings**:

1. The agent loop is `SessionPrompt.runLoop` at `session/prompt.ts:1244`;
   per-turn work is driven by `SessionProcessor` at
   `session/processor.ts:780` via AI SDK `streamText` (or an opt-in
   native runtime).
2. Compaction is a real LLM turn against a special `compaction` agent
   producing a fixed Markdown template, anchored by `tail_start_id` on
   a `compaction` part; messages aren't deleted, they're filtered at
   read time by `filterCompacted`
   (`session/compaction.ts:42-77, 245-294`; `message-v2.ts:1014-1064`).
3. opencode uses a `chars/4` tokenizer estimate (`util/token.ts:3`) for
   compaction decisions; authoritative token counts come from provider
   `Usage` on `step-finish` (`session/processor.ts:558-587`).
4. Prompt cache breakpoints are a fixed "first 2 system + last 2
   non-system" scheme applied only on Anthropic-family providers
   (`provider/transform.ts:345-394, 437-449`).
5. Subagents run via the `task` tool as fresh child sessions with
   derived permissions (`agent/subagent-permissions.ts:17`); they share
   the project DB but get only the explicit `prompt` text from the
   parent, not the parent's message history (`tool/task.ts:142-200`).
