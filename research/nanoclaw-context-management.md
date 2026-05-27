# NanoClaw — Context Management

Research target: `/Users/season/Personal/nanoclaw` (v2.0.64). Scope: how the
system assembles per-turn requests, compacts, persists memory, budgets tokens,
and isolates subagents.

> Note: the nanoclaw repo contains its own `research/nanoclaw-context-management.md`
> (a prior artifact inside the target tree). This report was written
> independently from source and cites `path:line` against the live code; it was
> not derived from that file.

---

## 1. Executive summary

**The headline: NanoClaw delegates almost all context management to the Claude
Agent SDK (headless Claude Code), and owns almost none of it directly.** The
container-side "agent-runner" is a thin message-pump that (a) batches inbound
chat rows out of a SQLite DB, (b) formats them into an XML prompt, (c) calls
`query()` from `@anthropic-ai/claude-agent-sdk` with a `resume` token, and (d)
parses the SDK's final text for `<message to="…">` blocks to deliver. Message
history, compaction, the system prompt scaffold, tokenization, and prompt-cache
are all internal to the SDK / Claude Code subprocess.

What NanoClaw *does* own:

- **Per-turn input assembly** — selecting and XML-formatting the most-recent ≤10
  pending messages (`formatter.ts`, `messages-in.ts`), plus a runtime
  system-prompt addendum (agent identity + destination map) appended to the
  `claude_code` preset.
- **Conversation continuity** — an opaque per-provider `continuation` (the SDK
  `session_id`) persisted in `outbound.db` and replayed via `resume:`; the
  actual transcript `.jsonl` is persisted by bind-mounting the SDK state dir to
  the host.
- **Compaction *policy* (not algorithm)** — it sets the auto-compact token
  window via env (`165000`), injects custom compaction instructions via a
  PreCompact shell hook, and archives the pre-compaction transcript to a
  markdown file.
- **File-based long-term memory** — `CLAUDE.local.md`, agent-built files, a
  `conversations/` archive, and a read-only `global/` dir, all surfaced through
  CLAUDE.md import composition.

Highest-confidence findings (each cited below): the loop is `runPollLoop`
(`poll-loop.ts:53`); history is SDK-resumed via `resume` (`claude.ts:292`); the
compaction window is `165000` tokens passed through env (`claude.ts:244,271`);
there is **no NanoClaw tokenizer and no NanoClaw `cache_control`** (grep
negative); there are **no custom subagent definitions** (`find` negative) —
"subagents" are either the SDK's experimental agent-teams (flag on, otherwise
unconfigured) or NanoClaw's own agent-to-agent messaging between separate
persistent containers.

---

## 2. End-to-end turn trace

The host (`src/`) and container agent-runner (`container/agent-runner/src/`) are
two separate runtimes that communicate **only** through two per-session SQLite
files — `inbound.db` (host writes, container reads) and `outbound.db` (container
writes, host reads) (`CLAUDE.md:43-51`). Context management lives entirely on the
container side; the host's role is to write inbound rows and read outbound rows.

A single turn, end to end:

1. **Bootstrap (per container).** `main()` loads `container.json`, builds the
   system-prompt addendum, discovers `/workspace/extra/*` extra dirs, assembles
   the MCP-server map (built-in `nanoclaw` server + any from config), constructs
   the provider, and calls `runPollLoop` (`index.ts:42-104`).

2. **Resume prior session.** The loop reads the persisted continuation for this
   provider and hands it to the provider unchanged:
   > "The continuation is opaque to the poll-loop — the provider decides how to
   > use it (Claude resumes a .jsonl transcript…)" (`poll-loop.ts:56-59`).
   It also clears stale `processing` acks left by a crashed container
   (`poll-loop.ts:67`).

3. **Poll + batch.** Each iteration calls `getPendingMessages(isFirstPoll)`,
   dropping `kind:'system'` rows (`poll-loop.ts:73`). The query returns the
   **most recent ≤`maxMessagesPerPrompt` (default 10)** pending rows
   `ORDER BY seq DESC LIMIT`, filters out rows already acked, then `reverse()`s
   to chronological order (`messages-in.ts:65-97`). An "accumulate gate" skips
   waking if the batch has no `trigger=1` row (`poll-loop.ts:95-98`).

4. **Command handling.** `/clear` is the only command the runner handles
   itself — it resets `continuation` and clears the persisted row
   (`poll-loop.ts:111-128`). Other admin/passthrough slash commands (e.g.
   `/compact`, `/context`) are passed to the SDK as **raw text** (no XML) so the
   SDK dispatches them natively (`poll-loop.ts:228-254`, `formatter.ts:14`).

5. **Format the prompt.** `formatMessages` prepends a
   `<context timezone="…"/>` header, then renders each row as
   `<message id sender time from>…</message>` / `<task>` / `<webhook>` /
   `<system_response>`, **stripping routing fields** (platform_id/channel_type/
   thread_id) so the model never sees them (`formatter.ts:129-223`). The prompt
   contains **only the new batch** — history is not re-sent.

6. **Call the SDK.** `provider.query({ prompt, continuation, cwd, systemContext })`
   (`poll-loop.ts:170-175`). The Claude provider creates a push-based
   `MessageStream`, pushes the batch as the first user turn, and calls `sdkQuery`
   with `resume: continuation`, the `claude_code` preset system prompt + appended
   instructions, the tool allowlist, MCP servers, hooks, model/effort, and
   `permissionMode:'bypassPermissions'` (`claude.ts:280-314`).

7. **Stream + concurrent follow-ups.** While the query runs, a 500ms timer polls
   for new messages and `query.push()`es them into the *same* open stream as new
   user turns — avoiding SDK subprocess re-spawn (`poll-loop.ts:281-356`). A
   pending slash command instead `query.end()`s so the outer loop can reopen
   (`poll-loop.ts:296-301`).

8. **Events → effects.** The provider translates SDK messages into
   `init`/`result`/`error`/`progress`/`activity` events (`claude.ts:318-346`).
   On `init` the new `session_id` is persisted **immediately** so a mid-turn
   crash still resumes (`poll-loop.ts:363-371`). On `result`, the batch is marked
   completed and the final text is parsed for `<message to="…">` blocks, each
   dispatched to a resolved destination; unwrapped text is treated as scratchpad
   and triggers a one-time corrective nudge pushed back into the stream
   (`poll-loop.ts:372-394`, `dispatchResultText` `:431-471`).

9. **Persist + loop.** The (possibly rotated) continuation is written back to
   `outbound.db` (`poll-loop.ts:185-188`), rows are marked completed, and the
   loop continues.

Compaction, if the window is exceeded, happens *inside* the SDK during step 7
and is surfaced to NanoClaw only as a `compact_boundary` system message
(`claude.ts:336-339`) and the two PreCompact hooks firing.

---

## 3. Subsystem findings

### 3.1 Per-turn request assembly

- **System prompt.** NanoClaw uses the SDK's built-in Claude Code preset and
  appends a runtime addendum:
  > `systemPrompt: instructions ? { type: 'preset', preset: 'claude_code', append: instructions } : undefined`
  (`claude.ts:293`).
  The appended `instructions` are built per turn by
  `buildSystemPromptAddendum(assistantName)` = agent identity + a live
  destinations section telling the model to wrap output in
  `<message to="name">` blocks (`destinations.ts:82-130`, assembled in
  `index.ts:54`). Identity is injected here (not in CLAUDE.md) because it's
  per-group and mutable (`destinations.ts:76-81`).

- **Project/user instruction files.** `settingSources: ['project','user']`
  (`claude.ts:305`) makes the SDK load `CLAUDE.md` from `cwd=/workspace/agent`.
  That file is **regenerated every spawn** as an imports-only entry by
  `composeGroupClaudeMd` (`claude-md-compose.ts:43-136`): it imports the shared
  base (`@./.claude-shared.md` → `/app/CLAUDE.md`), per-skill and per-MCP-tool
  `*.instructions.md` fragments, and inline MCP-server instructions; `CLAUDE.local.md`
  is auto-loaded separately by Claude Code (`claude-md-compose.ts:124-135`,
  `container/CLAUDE.md:1-21`).

- **Tool defs.** A static allowlist (`Bash, Read, Write, Edit, Glob, Grep,
  WebSearch, WebFetch, Task, TaskOutput, TaskStop, TeamCreate, TeamDelete,
  SendMessage, TodoWrite, ToolSearch, Skill, NotebookEdit`) is unioned with one
  wildcard per MCP server (`mcp__<server>__*`) (`claude.ts:42-68, 294-297`). A
  disallow-list blocks SDK builtins that don't fit the headless async model
  (`CronCreate/Delete/List, ScheduleWakeup, AskUserQuestion, Enter/ExitPlanMode,
  Enter/ExitWorktree`) — NanoClaw provides MCP equivalents
  (`claude.ts:25-35, 298`), with a `PreToolUse` hook as defense-in-depth
  (`claude.ts:160-179`).

- **Message history.** Not assembled by NanoClaw. It is the SDK transcript
  resumed via `resume: input.continuation` (`claude.ts:292`). The per-turn
  `prompt` carries only the new batch (see trace step 5).

- **Injected context.** Per turn: the `<context timezone>` header
  (`formatter.ts:130`), the identity+destinations addendum, and any
  attachment/reply context rendered inline (`formatter.ts:235-257`). Standing
  context: CLAUDE.md imports + `CLAUDE.local.md` + the read-only `global/` dir.

- **Prompt-cache breakpoints.** **None set by NanoClaw.** Grep for
  `cache_control`/`cacheControl`/`ephemeral` across `container/agent-runner/src`
  and `src` returns nothing. Caching is the SDK/server's concern; the loop
  explicitly relies on the server-side 5-min prefix cache:
  > "The Anthropic prompt cache is server-side with a 5-min TTL keyed on prefix
  > hash, so stream lifecycle does NOT affect cache lifetime" (`poll-loop.ts:273-275`).

### 3.2 Compaction

- **Trigger.** Auto-compaction is the SDK's, fired at a token window NanoClaw
  configures via env:
  > `const CLAUDE_CODE_AUTO_COMPACT_WINDOW = process.env.CLAUDE_CODE_AUTO_COMPACT_WINDOW || '165000'`
  (`claude.ts:244`), injected into the SDK subprocess env (`claude.ts:271`).
  Operators can override it without editing source (`claude.ts:240-243`).
  On-demand compaction is the native `/compact` admin command passed through as
  raw text (`formatter.ts:14`, `poll-loop.ts:233-244`).

- **Algorithm.** Owned by the SDK / Claude Code — NanoClaw does not generate the
  summary. Host-side has no compaction/summarization logic (grep of `src/`
  finds only the `/compact` admin-command string and the PreCompact hook
  wiring). NanoClaw influences the summary via a **PreCompact shell hook** whose
  stdout becomes the SDK's `customInstructions`:
  > "Claude Code captures the stdout of PreCompact shell hooks and passes it as
  > `customInstructions` to the compaction prompt" (`compact-instructions.ts:4-7`).
  Those instructions tell the summarizer to preserve recent `<message>/<task>/
  <webhook>` XML tags + attributes, chronological order, and to re-append the
  "wrap responses in `<message to=…>`" reminder with the live destination list
  (`compact-instructions.ts:17-34`). The hook is wired into
  `.claude-shared/settings.json` at group-init (`group-init.ts:18-28, 106-133`),
  which the SDK loads as the *user* settings source (that dir mounts to
  `/home/node/.claude`, see 3.3).

- **Preserved vs dropped.** The compacted summary replaces older turns inside
  the SDK transcript; recent message XML is preserved by the custom
  instructions; everything else is at the SDK summarizer's discretion. NanoClaw
  surfaces the event as a synthetic result line ("Context compacted (N tokens
  compacted).") (`claude.ts:336-339`).

- **Where the summary lands.** Inside the SDK-managed `.jsonl` transcript — the
  continuation/resume target. NanoClaw does **not** store the summary itself.
  Separately, a **second** PreCompact hook (registered in-SDK, not via settings)
  archives the *pre*-compaction transcript to markdown for human/agent recall:
  `createPreCompactHook` reads `transcript_path`, parses user/assistant text,
  truncates each message to 2000 chars, and writes
  `/workspace/agent/conversations/<date>-<slug>.md` (`claude.ts:191-232`). This
  archive is a side-channel, not part of the live context window.

### 3.3 Memory

- **In-session state.** The opaque `continuation` (SDK `session_id`) is the
  in-session handle. It is persisted to `outbound.db`'s `session_state` table,
  keyed per provider so flipping providers never resurrects a stale id
  (`session-state.ts:1-79`). It is written on the SDK `init` event (not just at
  turn end) so mid-turn crashes still resume (`poll-loop.ts:363-371`). Module-
  level `currentInReplyTo` carries the batch's reply id to MCP tools within one
  turn (`current-batch.ts:16-28`).

- **Cross-session persistence.** Three layers:
  1. **SDK transcript.** `/home/node/.claude` (SDK state + `projects/*.jsonl`)
     is bind-mounted to a host dir per agent group:
     `mounts.push({ hostPath: claudeDir, containerPath: '/home/node/.claude', readonly: false })`
     (`container-runner.ts:256, 311`). Because it's host-backed, resuming the
     continuation after a container exits replays the real transcript.
  2. **File-based memory.** `CLAUDE.local.md` is per-group memory, auto-loaded by
     Claude Code and seeded at init (`group-init.ts:59-66`); the shared base
     instructs the agent to record substantive facts there and to build its own
     structured files (e.g. `customers.md`) referenced from `CLAUDE.local.md`
     (`container/CLAUDE.md:13-21`). A read-only `groups/global/` dir mounts at
     `/workspace/global` for shared memory (`container-runner.ts:296-300`).
  3. **Conversation archive.** `conversations/*.md` written by the PreCompact
     archive hook (3.2), described to the agent as "searchable transcripts of
     past sessions" (`container/CLAUDE.md:19-21`).

- **Auto-memory flag.** `CLAUDE_CODE_DISABLE_AUTO_MEMORY: '0'` and
  `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD: '1'` are set in the scaffolded
  settings (`group-init.ts:14-15`), leaving the SDK's own memory affordances on.

### 3.4 Token budgeting

- **Tokenizer.** None of NanoClaw's own. Grep for
  `tokeniz|tiktoken|token_count|countTokens|max_tokens` across both source trees
  returns nothing. All token accounting is internal to the SDK/model.

- **Per-turn split / the only knob.** The single explicit budget control is the
  `165000` auto-compact window (3.2). There is no NanoClaw-side reserve for
  system/tools/history — the SDK manages the window.

- **Input backpressure.** The closest thing to backpressure on context is the
  message-batch cap: at most `maxMessagesPerPrompt` (default 10) pending rows per
  turn, taken as the *most recent* by `ORDER BY seq DESC LIMIT`
  (`messages-in.ts:45-52, 65-97`; default in `config.ts:23,46`). Surplus pending
  rows are not dropped — they ride along on subsequent turns. Once processed,
  history volume is bounded only by SDK compaction.

- **Other truncation.** The PreCompact markdown archive truncates each message
  to 2000 chars (`claude.ts:148`); this affects the archive file only, not the
  live window.

- **Liveness backpressure (not token-based).** The host sweep kills a container
  whose heartbeat file is older than `ABSOLUTE_CEILING_MS = 30min` or whose
  `processing` claim exceeds `CLAIM_STUCK_MS = 60s` with no heartbeat progress,
  resetting its rows to pending (`host-sweep.ts:61-68`; design notes `:1-25`).
  This guards stuck/looping turns, not context size.

### 3.5 Subagents

- **No NanoClaw-defined subagents.** `find` for `.claude/agents/*` and a dir
  named `agents` returns nothing in the repo; there is no custom agent
  definition, isolation, inheritance, or reintegration logic in agent-runner.

- **SDK agent-teams: enabled but unconfigured.**
  `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS: '1'` is set (`group-init.ts:13`) and the
  `Task/TaskOutput/TaskStop/TeamCreate/TeamDelete/SendMessage` tools are
  allowlisted (`claude.ts:52-57`), so the SDK's experimental teams feature is
  available to the model — but NanoClaw provides no team/agent definitions or
  reintegration handling. Any isolation/merging is whatever the SDK does
  internally.

- **NanoClaw's actual multi-agent model is agent-to-agent (a2a) messaging
  between *separate persistent containers*, not in-process subagents.**
  Destinations of `type:'agent'` resolve to another agent group's id
  (`destinations.ts:15-72`); a `<message to="otherAgent">` block writes an
  outbound row addressed to that agent group, which the host routes into the
  other group's `inbound.db` (`poll-loop.ts:473-490`). Each agent group is its
  own container with its own session DBs, CLAUDE.md, and SDK transcript — full
  isolation by construction, with "reintegration" being ordinary message
  delivery, not context merging.

---

## 4. Non-obvious design choices

- **SDK-as-context-engine.** By resuming a `.jsonl` transcript via an opaque
  `continuation`, NanoClaw outsources history, compaction, and tokenization
  entirely. The agent-runner's prompt is *always just the new messages*; it
  never reconstructs history (`poll-loop.ts:170-175`, `claude.ts:292`). This
  keeps the runner tiny and provider-agnostic (`providers/types.ts`).

- **Transcript persistence via bind mount, not export.** Cross-restart memory is
  achieved by mounting the SDK's `~/.claude` to the host
  (`container-runner.ts:311`) rather than serializing state — the container is
  disposable (`--rm`) but its conversation survives.

- **Continuation persisted on `init`, keyed per provider.** Writing the session
  id the moment the SDK emits it (`poll-loop.ts:363-371`) closes the mid-turn-
  crash gap; per-provider keys (`session-state.ts:16,52-67`) make provider
  switching lossless.

- **PreCompact does double duty.** The same SDK event drives both an *archival*
  hook (markdown snapshot, `claude.ts:191-232`) and a *steering* hook (stdout →
  `customInstructions`, `compact-instructions.ts`). The two are registered
  through different channels (SDK `hooks` option vs settings.json), so both fire.

- **Routing stripped from context, identity injected at runtime.** The model
  never sees platform/thread ids (`formatter.ts:128`); instead each inbound tag
  carries a friendly `from="name"` and the system addendum lists destinations —
  routing is reframed as named addressing (`destinations.ts:94-129`).

- **Keep the stream open across turns.** Follow-ups are pushed into a live query
  rather than re-spawning the SDK, explicitly reasoned against the server-side
  cache TTL (`poll-loop.ts:270-356`).

---

## 5. Under-developed or risky areas

- **Compaction is a black box NanoClaw can only nudge.** The summary algorithm,
  what it keeps, and its token target are all SDK-internal. NanoClaw's only
  levers are the `165000` window and best-effort `customInstructions`
  (`compact-instructions.ts`). If a future SDK version changes hook semantics or
  preset behavior, context fidelity changes silently.

- **`customInstructions` preservation is hope-based.** The hook *requests* that
  recent XML and the wrapping reminder survive compaction
  (`compact-instructions.ts:17-34`); nothing verifies the summarizer complied. A
  bad compaction could drop the `<message to=…>` contract mid-conversation —
  partially mitigated at runtime by the unwrapped-response nudge
  (`poll-loop.ts:380-392`).

- **Lost container logs.** Containers run `--rm`; if the agent fails silently
  inside the container there's no persistent log (`CLAUDE.md:264`). Debugging a
  context/compaction misfire relies on the session DBs and archived markdown.

- **Batch cap can reorder perceived recency under load.** `ORDER BY seq DESC
  LIMIT 10` then `reverse()` means a burst >10 messages delivers only the newest
  10 this turn; older-but-unprocessed rows arrive on later turns, so the model
  can see messages slightly out of arrival order across turns
  (`messages-in.ts:74-93`).

- **Stale-continuation recovery is regex-based.** Invalid-session detection
  matches SDK error text (`STALE_SESSION_RE`, `claude.ts:251`); an SDK wording
  change would break auto-recovery and strand a bad continuation until `/clear`.

- **Agent-teams flag on without governance.** `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`
  exposes Task/Team tooling (`group-init.ts:13`, `claude.ts:52-57`) with no
  NanoClaw-side budget or isolation policy — sub-task token usage is invisible to
  NanoClaw's accounting (which is itself just the one compaction window).

---

## 6. Open questions / confidence gaps

- **High confidence:** loop entry point, SDK delegation, resume/continuation
  persistence, compaction trigger value, two PreCompact hooks, file-based memory
  layers, absence of own tokenizer / cache_control / host compaction, absence of
  custom subagents. All cited against code read in full.

- **Medium confidence — SDK internals.** Exactly how Claude Code builds the
  request (cache breakpoint placement, system/tool ordering, where the compaction
  summary is inserted in the transcript) is inside `@anthropic-ai/claude-agent-sdk`
  (`container/agent-runner/package.json` pins `^0.2.128`) and the
  `/pnpm/claude` executable (`claude.ts:292`); not inspected here. Confirming
  would require reading the SDK package or the bundled Claude Code binary.

- **Gap — does the shell PreCompact hook actually reach the SDK?** I confirmed it
  is written to `.claude-shared/settings.json` (`group-init.ts:18-28`) and that
  dir mounts to `/home/node/.claude` (`container-runner.ts:311`), and that
  `settingSources` includes `'user'` (`claude.ts:305`). The remaining assumption
  is that the SDK treats `$HOME/.claude/settings.json` as the `user` source and
  honors `command`-type PreCompact hooks for stdout→`customInstructions`; this
  matches the documented Claude Code behavior the comment relies on
  (`compact-instructions.ts:4-10`) but was not verified against the SDK binary.

- **Gap — agent-teams runtime behavior.** Whether the model actually spawns SDK
  teams in practice, and how the SDK isolates their context, is unobserved; the
  code only enables the capability.

- **Not re-verified:** the `docs/SDK_DEEP_DIVE.md` / `docs/agent-runner-details.md`
  writeups exist and likely corroborate this, but per the brief I traced behavior
  from source rather than from prose docs.
