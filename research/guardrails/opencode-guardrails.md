# OpenCode Guardrails Research Report

**Project:** `anomalyco/opencode` — "The open source AI coding agent"
**Repo path:** `/Users/season/Personal/opencode`
**Date:** 2025-05-27

---

## 1. Executive Summary

OpenCode implements a layered, declarative permission system built on Effect-ts, with a ruleset-evaluation model (allow/deny/ask) that governs every tool invocation. The architecture is agent-centric: each named agent (build, plan, explore, scout, general, compaction, title, summary) carries its own permission ruleset. These rules merge with user config and per-session overrides. There is no sandboxing, no container isolation, and no explicit prompt-injection defence layer — guardrails are primarily permission gates with human-in-the-loop approval flows.

**Key strengths:** Unified permission model, doom-loop detection, snapshot/revert, subagent permission inheritance, external-directory boundary.

**Key gaps:** No sandboxing or container isolation, no explicit prompt-injection hardening, no secrets redaction, no output validation layer, telemetry opt-in not verified.

---

## 2. End-to-End Guarded Turn Trace

1. **User sends a message** → `SessionPrompt.prompt()` (`session/prompt.ts:1215`)
2. **Session fetched** and revert cleaned up if pending (`session/prompt.ts:1217-1218`)
3. **User message created** with agent resolution, model selection, and part resolution (`session/prompt.ts:1219`)
4. **Per-session tool overrides** applied via `setPermission` (`session/prompt.ts:1222-1228`)
5. **Loop entered** via `runLoop` (`session/prompt.ts:1301`) — a `while(true)` loop that continues until stop/compact
6. **System prompt assembled** from: environment info, instruction files (AGENTS.md, CLAUDE.md, URLs), skills, agent-specific prompt, model-specific prompt variant (`session/system.ts`, `session/instruction.ts`)
7. **Tools resolved** via `SessionTools.resolve()` (`session/tools.ts`) — builds AI SDK tool wrappers that:
   - Validate parameters via Effect Schema `decodeUnknownEffect`
   - Call `ctx.ask()` before execution which invokes `Permission.ask()`
   - Fire plugin hooks `tool.execute.before` / `tool.execute.after`
8. **LLM stream created** via `LLM.stream()` (`session/llm.ts`) — sends system + messages + tools to provider
9. **Stream events processed** by `SessionProcessor.handleEvent()` (`session/processor.ts`) — handles tool-call, tool-result, tool-error, reasoning, text events
10. **On tool-call event:** doom-loop detection (3 identical calls → ask user), then permission check embedded in tool's `execute()`
11. **Tool executes** → `ctx.ask()` evaluates the merged ruleset. If action is `ask`, a deferred is created and published to the bus. UI shows approval prompt. User can approve once, always, or reject.
12. **On rejection:** `Permission.RejectedError` or `CorrectedError` thrown, tool call marked as error, session may stop depending on `experimental.continue_loop_on_deny` flag
13. **Snapshot tracked** before each step; diff computed after for revert capability
14. **Loop continues** until stop, compact, or max steps reached

---

## 3. Guardrail Subsystem Findings

### 3.1 Instruction Hierarchy

**System prompt construction order** (`session/prompt.ts:1404-1414`):
```
system = [...env, ...instructions, ...(skills ? [skills] : [])]
```

Where:
- `env`: Model identity, working directory, platform, date (`session/system.ts:23-38`)
- `instructions`: Loaded from AGENTS.md, CLAUDE.md (unless `disableClaudeCodePrompt` flag), CONTEXT.md, user-configured instruction URLs (`session/instruction.ts`)
- `skills`: Available skill descriptions appended if `skill` tool isn't denied (`session/system.ts:49-56`)
- **Agent-specific prompt**: Each agent has an optional `prompt` field (e.g., `PROMPT_EXPLORE`, `PROMPT_SCOUT`)
- **Model-specific prompt**: `SystemPrompt.provider()` selects a model-specific system prompt template (anthropic.txt, gpt.txt, etc.) — this is used in the `generate` agent endpoint but the main loop uses the agent's `prompt` field

**File discovery order** (`session/instruction.ts`):
1. Global: `~/.config/opencode/AGENTS.md` (first match wins)
2. `~/.claude/CLAUDE.md` (unless disabled)
3. Project: walks up from CWD to worktree finding first AGENTS.md/CLAUDE.md
4. Config `instructions` entries (local paths globbed, URLs fetched with 5s timeout)

**Precedence:** No explicit override rules between system prompt layers. They are concatenated. User config in `opencode.json` `permission` section takes precedence in the permission layer (merged last). There is no conflict resolution — the merge is append-based.

### 3.2 Tool Guardrails

**Permission model** (`permission/index.ts`, `core/permission.ts`):

Every tool call flows through `ctx.ask()` which calls `Permission.ask()`. The evaluation:
```typescript
// core/permission.ts:25-32
rulesets.flat().findLast(rule => 
  Wildcard.match(permission, rule.permission) && 
  Wildcard.match(pattern, rule.pattern)
)
```
Default action if no rule matches: `"ask"`.

Three outcomes:
- `"allow"` → proceed silently
- `"deny"` → `DeniedError` thrown immediately, tool not executed
- `"ask"` → deferred created, event published to bus, UI prompts user

User approval responses: `"once"`, `"always"`, `"reject"`. "Always" adds persistent rules to the session's approved list.

**Per-agent tool availability** (`agent/agent.ts`):
- **build** (default): All tools allowed except `doom_loop` (ask), `question` (deny), `plan_enter`/`plan_exit` (deny), `repo_clone`/`repo_overview` (deny), `.env` files (ask)
- **plan**: Edit tools denied, can only write to `.opencode/plans/`
- **explore**: Only read-only tools (grep, glob, list, bash, webfetch, websearch, read)
- **scout**: Read-only + repo_clone + repo_overview
- **general**: All except `todowrite` (deny)
- **compaction/title/summary**: All tools denied (`"*": "deny"`)

**Shell tool** (`tool/shell.ts`):
- Parses command via tree-sitter (bash and PowerShell grammars)
- Extracts file-affecting commands (rm, cp, mv, etc.) and resolves their path arguments
- If any argument resolves outside the worktree, triggers `external_directory` permission ask
- Separately asks for the shell command itself with arity-based pattern (e.g., `"git commit *"`)
- Timeout defaults to 2 minutes (`RuntimeFlags.bashDefaultTimeoutMs`)

**MCP tools** (`session/tools.ts:68-130`):
- All MCP tool calls require a blanket permission check: `ctx.ask({ permission: key, patterns: ["*"] })`
- No per-argument permission granularity for MCP tools

**Write/Edit/ApplyPatch tools**:
- All resolve the target path and call `assertExternalDirectoryEffect()` which triggers `external_directory` permission if path is outside worktree
- Then ask `edit` permission with the file diff in metadata
- Edit tool uses file-level semaphore to prevent concurrent writes to the same file

**WebFetch** (`tool/webfetch.ts`):
- Validates URL scheme (http/https only)
- Asks `webfetch` permission with URL as pattern
- Enforces 5MB response size limit
- Max timeout 120 seconds

**Read tool** (`tool/read.ts`):
- Asks `external_directory` permission if outside worktree
- Asks `read` permission with the file path
- `.env` files require explicit approval by default (agent permission config: `*.env: "ask"`)
- Binary files rejected
- Auto-loads nearby instruction files (AGENTS.md) as `<system-reminder>` context

### 3.3 Filesystem and Git Safety

**Write boundary** (`tool/external-directory.ts`):
- `containsPath()` checks if a file is within `ctx.directory` or `ctx.worktree`
- If outside, `external_directory` permission is triggered for the parent directory glob
- Non-git projects set worktree to `/` which disables the worktree check to avoid matching everything

**Protected directories** (`file/protected.ts`):
- Platform-specific sensitive directories are enumerated (macOS: Desktop, Documents, Downloads, Library subdirs; Windows: AppData, etc.)
- Used for directory scanning skip lists, not as permission deny-lists

**File ignore patterns** (`file/ignore.ts`):
- Skips node_modules, .git, build artifacts, etc. during file operations
- Not a security boundary — purely for performance

**Snapshot/revert system** (`snapshot/index.ts`):
- Git-based snapshot tracking: `git stash create` for snapshots
- `git apply --reverse` for reverting
- `SessionRevert` allows users to undo agent actions back to any message
- Revert requires session to not be busy (`assertNotBusy`)

**Git operations:**
- No explicit dirty-worktree protection
- No commit/push constraints — these are mediated through the shell tool's permission system
- No branch protection

### 3.4 Prompt-Injection Resistance

**No explicit defences found.** Specifically:

- **File content**: Read tool injects file content directly into the conversation. No sanitization or marking of untrusted content.
- **Web content**: WebFetch returns HTML converted to markdown or raw text. No sandboxing of fetched content.
- **MCP tool outputs**: Passed through directly.
- **Instruction files**: AGENTS.md/CLAUDE.md content is concatenated into the system prompt without isolation markers.
- **Tool output truncation**: `Truncate` service caps output size but doesn't sanitize content.

**Indirect mitigations:**
- The `external_directory` permission gate means the agent can't read arbitrary files without approval
- `.env` files require explicit approval
- Protected directories are skipped during scanning

### 3.5 Data Protection

**Secrets/Credentials:**
- API keys stored in config (`opencode.json`) or environment variables
- `.env` files require explicit permission to read (`*.env: "ask"`)
- No runtime secret redaction in tool outputs or logs
- No secret scanning before sending context to LLM providers

**Logging:**
- Effect-based structured logging throughout (`@opencode-ai/core/util/log`)
- Log content includes session IDs, tool names, model IDs — not full message content in most paths
- Shell tool metadata includes truncated command output in permission metadata

**Telemetry:**
- OpenTelemetry support is experimental (`experimental.openTelemetry` config flag)
- When enabled, traces include userId from config
- No evidence of a separate telemetry pipeline or opt-out mechanism

**Persistence:**
- Sessions stored in local SQLite via Drizzle ORM
- Permission approvals persisted per-project in `PermissionTable`
- Snapshots stored in git stash
- No encryption at rest

### 3.6 Runtime Validation

**Schema validation** (`tool/tool.ts`):
- Every tool parameter is validated via `Schema.decodeUnknownEffect(parameters)` before execution
- Invalid input produces `InvalidArgumentsError` which is fed back to the model as a tool result

**Permission evaluation** (`permission/index.ts`):
- `Permission.ask()` evaluates each pattern against rulesets + session-approved rules
- Deny → immediate error; Allow → skip; no match → ask user

**Doom-loop detection** (`session/processor.ts:25-27, 203-222`):
```typescript
const DOOM_LOOP_THRESHOLD = 3
```
If the last 3 tool calls are identical (same tool name, same JSON input, all non-pending), a `doom_loop` permission is asked. User can approve or reject.

**Retry logic** (`session/retry.ts`):
- `SessionRetry.policy()` handles retryable errors (rate limits, provider errors)
- Not configurable by the user

**Overflow/compaction** (`session/compaction.ts`):
- When token usage exceeds model limits, automatic compaction is triggered
- Compaction summarizes conversation history

**Output validation:** None. Tool outputs are passed directly to the LLM as tool results. No content filtering, no output schema validation.

### 3.7 Human-in-the-Loop Controls

**Approval points:**
- Every tool call with `ask` permission creates a deferred and publishes to the event bus
- UI receives `permission.asked` event and shows approval dialog
- User can: approve once, approve always (persists for session), reject, or reject with feedback

**Cancel/interrupt:**
- `SessionPrompt.cancel()` sets abort signal
- All tool executions respect `ctx.abort`
- Shell processes killed with `forceKillAfter: "3 seconds"`
- Background jobs cancelled on session cancel (`run-state.ts`)

**Revert:**
- `SessionRevert.revert()` undoes all file changes since a given message using git snapshots
- `SessionRevert.unrevert()` restores reverted state
- Revert requires session to be idle

**Status reporting:**
- `SessionStatus` tracks: idle, busy, retry
- Published via bus events for UI consumption

**Max steps:**
- Agents can define `steps` limit (optional)
- When reached, a `MAX_STEPS` message is injected as assistant prefill to force stop

### 3.8 Subagents

**Task tool** (`tool/task.ts`):
- Subagents spawned via the `task` tool
- Creates a child session with derived permissions

**Permission derivation** (`agent/subagent-permissions.ts`):
```typescript
export function deriveSubagentSessionPermission(input: {
  parentSessionPermission: Permission.Ruleset
  parentAgent: Agent.Info | undefined
  subagent: Agent.Info
}): Permission.Ruleset
```
Rules inherited:
1. Parent **agent's** edit-class deny rules (prevents plan-mode subagent bypass)
2. Parent **session's** deny rules and `external_directory` rules
3. Default `todowrite` and `task` denies unless the subagent's own ruleset explicitly allows them

**Isolation:**
- Subagents run in separate sessions with their own message history
- Background subagents run as `BackgroundJob` entries
- Foreground subagents block until complete
- No sandboxing — subagents share the same filesystem and process space

**Reintegration:**
- Foreground: task output injected as tool result
- Background: on completion, a synthetic message is injected into the parent session with the result

---

## 4. Non-Obvious Design Choices

1. **Permission rules are append-only, evaluated with `findLast`** (`core/permission.ts:27`). This means user config always overrides defaults because it's merged last, but there's no explicit conflict resolution — the last matching rule wins.

2. **`containsPath` special-cases non-git projects** (`project/instance-context.ts:20-22`). When worktree is `/` (non-git), the worktree check is skipped to avoid matching every absolute path, which would silently bypass `external_directory` permissions.

3. **Shell tool parses with tree-sitter** (`tool/shell.ts`). Commands are parsed via bash/PowerShell grammars to extract file paths for permission gating, not just string matching. This is unusually thorough but also complex.

4. **`.env` protection is pattern-based** (`agent/agent.ts:67-71`). `*.env` and `*.env.*` require `ask`, but `*.env.example` is explicitly allowed. Uses glob-style matching, not content-based detection.

5. **Instruction auto-loading** (`tool/read.ts` + `session/instruction.ts`). When reading a file, the system walks upward looking for AGENTS.md files that haven't been loaded yet and injects them as `<system-reminder>` blocks. This is a "proximal context" feature.

6. **Doom-loop threshold is exactly 3** (`session/processor.ts:25`). If three consecutive identical tool calls are detected, it asks for permission. This is the only built-in loop detection.

7. **Plugin hooks wrap every tool execution** (`session/tools.ts:44-48, 60-64`). `tool.execute.before` and `tool.execute.after` hooks allow plugins to observe and potentially modify tool behavior, but there's no plugin permission model.

8. **MCP tools get blanket permission** (`session/tools.ts:97`). All MCP tool calls ask with `patterns: ["*"]` — no per-argument granularity, unlike native tools.

9. **GitLab Workflow models have special approval handling** (`session/llm.ts:105-168`). Server-side tool calls from GitLab workflow models have a separate approval flow via `approvalHandler`.

---

## 5. Under-Developed or Risky Areas

1. **No sandboxing or isolation** — Shell commands run as the user's full-privilege process. There is no container, chroot, or capability restriction. A malicious or confused model can execute arbitrary commands.

2. **No prompt-injection defenses** — File content, web content, and MCP tool outputs are injected directly into the LLM context without isolation markers or content classification. A crafted file could manipulate agent behavior.

3. **No secrets redaction** — Tool outputs (including shell output) are sent to the LLM provider without scanning for secrets. The `.env` read permission is the only mitigation, and it's pattern-based.

4. **`external_directory` can be permanently allowed** — The "always" approval for external directory access persists for the session with no expiration or scope narrowing.

5. **No output validation** — Tool outputs are forwarded to the LLM without content filtering. There's no check for exfiltration patterns in tool results.

6. **Plugin system has no permission model** — Plugins can transform system prompts, messages, and tool behavior via hooks, but there's no restriction on what plugins can do.

7. **Compaction agent has all tools denied** but no separate validation — The compaction agent is restricted to `deny` on `*`, but this is enforced through the same permission system as regular tools, not through a harder boundary.

8. **Remote instruction URLs** — Config supports loading instructions from HTTP URLs (`session/instruction.ts:91-96`) with a 5s timeout. These are fetched and concatenated into the system prompt without integrity checks.

9. **Background subagents have limited oversight** — Once launched, background tasks run to completion. The only control is cancellation. No intermediate approval points.

---

## 6. Open Questions / Confidence Gaps

1. **Plugin permission model** — The plugin system (`plugin/`) was not deeply analyzed. Plugins have hooks that can transform prompts and messages. It's unclear what constraints exist on plugin behavior.

2. **Enterprise package** — `packages/enterprise` exists but was not examined. May contain additional guardrails for enterprise deployments.

3. **Desktop app sandboxing** — The desktop app (`packages/desktop`) may add additional OS-level restrictions. Not analyzed.

4. **Config server** — `config/server.ts` exists. May allow remote config loading which could be a guardrail vector. Not deeply analyzed.

5. **ACPs (Agent Communication Protocol)** — `src/acp` and `src/acp-next` directories exist. May have their own permission model. Not analyzed.

6. **Confidence in "no prompt-injection defense" claim** — I searched for injection-related patterns but the codebase is large. There may be defenses in model-specific prompt templates that I didn't fully trace.

7. **Telemetry and data collection** — The `stats` package and `STATS.md` were not examined. The extent of data collection is unknown from this analysis.

---

*End of report.*
