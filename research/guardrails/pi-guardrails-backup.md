# Pi — Guardrails (permissions, sandboxing, approval policy)

Source tree: `/Users/season/Personal/pi` (monorepo `earendil-works/pi-mono`).
Primary packages inspected:
- `packages/coding-agent` (`@earendil-works/pi-coding-agent`): the CLI harness — tools, session, extension bus, modes.
- `packages/agent` (`@earendil-works/pi-agent-core`): the agent loop where tool calls are validated and dispatched.
- `packages/ai` (`@earendil-works/pi-ai`): provider transport + the only argument validator (`validateToolArguments`).

Framing for nav (single-operator local coding agent): the question is what protects the operator's machine when the model drives `bash`/`write`/`edit`. The headline finding holds up under the code: **pi ships no OS sandbox and no built-in human-approval gate for tool calls.** The only place a guard *can* sit on the model's own tool calls is the `beforeToolCall` hook in the agent loop, which the coding agent wires to the extension `tool_call` event — and pi ships zero extensions that use it. Everything below is grounded in the source.

## 1. Executive summary

- **No OS sandbox of any kind.** No seatbelt / `sandbox-exec` / landlock / seccomp / bubblewrap / firejail anywhere in the agent or coding-agent source. `bash` spawns a real login-shell child process directly via `child_process.spawn` with the full inherited environment (`packages/coding-agent/src/core/tools/bash.ts:79-85`). The only files matching "sandbox" are about pi *running inside* someone else's sandbox (recovering env from `/proc/self/environ`), not creating one (`packages/coding-agent/src/bun/restore-sandbox-env.ts:11-32`).
- **No built-in approval RPC / permission popup.** Confirmed against both code and docs. The docs state it as a design choice: pi "intentionally does not include built-in MCP, sub-agents, permission popups, plan mode, to-dos, or background bash" (`packages/coding-agent/docs/usage.md:277`). No `yolo`, `--dangerously-*`, `approvalPolicy`, `permissionMode`, or `autoApprove` symbols exist in the coding-agent source.
- **The one interception seam is `beforeToolCall`.** The loop calls `config.beforeToolCall(...)` after schema validation and before execution; returning `{ block: true }` turns into an error tool result (`packages/agent/src/agent-loop.ts:581-605`). The coding agent forwards this to the extension `tool_call` event (`packages/coding-agent/src/core/agent-session.ts:397-416`). It is a *hook*, not a built-in policy engine — and no shipped extension uses it. Approval/path-protection are explicitly left to userland extensions (`packages/coding-agent/docs/extensions.md:19-21, 2541-2542`).
- **No path/workspace containment.** `write`, `edit`, and `read` resolve the model-supplied path against cwd (`resolveToCwd` → `resolvePath`) and then operate on it directly. Absolute paths and `..` escapes resolve normally and are honored; there is no in-workspace check, no writable-root allowlist, and no symlink-escape rejection (`packages/coding-agent/src/core/tools/write.ts:201-218`, `edit.ts:312-351`, `read.ts:127`, `packages/coding-agent/src/utils/paths.ts:81-85`).
- **No destructive-action detection.** No pattern match for `rm -rf`, `sudo`, force-push, etc. anywhere in tool code. The only place such a check exists is a *documentation example* of an extension a user would write (`packages/coding-agent/docs/extensions.md:69-72, 703-705`).
- **Argument validation is shape-only.** `validateToolArguments` does TypeBox/JSON-schema validation + coercion and nothing semantic; on schema-compile failure in restricted runtimes it falls back to running unvalidated (`packages/ai/src/utils/validation.ts:292-324`; `packages/ai/CHANGELOG.md` notes the Cloudflare-Workers fallback).
- **Posture is "trust the operator and the extensions."** The extensions doc is blunt: "Extensions run with your full system permissions and can execute arbitrary code. Only install from sources you trust." (`packages/coding-agent/docs/extensions.md`, Security note). Guardrails are an opt-in extension responsibility, not a harness feature.

## 2. End-to-end trace: a `bash` tool call from model output to side effect

This traces a model-emitted `bash` call through validation and execution to the actual `spawn`. Establishes exactly where a guard *could* sit and what *actually* runs.

### 2.1 Loop entry: prepare → validate → beforeToolCall → execute → finalize

After the assistant message streams in, `runLoop` finds `toolCall` blocks and dispatches each through three stages (`packages/agent/src/agent-loop.ts`). Stage 1 is `prepareToolCall` (`agent-loop.ts:562-626`):

```ts
const tool = currentContext.tools?.find((t) => t.name === toolCall.name);
if (!tool) { return { kind: "immediate", result: createErrorToolResult(`Tool ${toolCall.name} not found`), isError: true }; }
try {
  const preparedToolCall = prepareToolCallArguments(tool, toolCall);
  const validatedArgs = validateToolArguments(tool, preparedToolCall);   // shape-only
  if (config.beforeToolCall) {
    const beforeResult = await config.beforeToolCall({ assistantMessage, toolCall, args: validatedArgs, context: currentContext }, signal);
    ...
    if (beforeResult?.block) {
      return { kind: "immediate", result: createErrorToolResult(beforeResult.reason || "Tool execution was blocked"), isError: true };
    }
  }
  return { kind: "prepared", toolCall, tool, args: validatedArgs };
```

This is the only choke point. There are exactly two guard surfaces here:
1. `validateToolArguments(tool, preparedToolCall)` (`agent-loop.ts:580`) — TypeBox schema check only (§3.6).
2. `config.beforeToolCall(...)` (`agent-loop.ts:582-590`) — the optional hook. If it's `undefined`, execution proceeds with no gate at all.

Stage 2, `executePreparedToolCall`, calls `prepared.tool.execute(...)` (`agent-loop.ts:628-663`). Stage 3, `finalizeExecutedToolCall`, runs `config.afterToolCall` which can rewrite/flag the result but **cannot un-run a side effect** (`agent-loop.ts:665-708`) — by the time it fires, `bash`/`write` have already touched the system.

A `ToolResultMessage` is then appended and emitted via `message_start`/`message_end` (`agent-loop.ts:727-742`). A blocked call surfaces to the model as an ordinary error tool result (`createErrorToolResult`, `agent-loop.ts:710-714`), and to the UI as an errored tool render — there is no distinct "pending/awaiting-approval" state in the loop.

### 2.2 Default wiring: no guard unless an extension installs one

The coding agent installs `beforeToolCall`/`afterToolCall` once, reading the live extension runner at call time (`packages/coding-agent/src/core/agent-session.ts:396-416`):

```ts
this.agent.beforeToolCall = async ({ toolCall, args }) => {
  const runner = this._extensionRunner;
  if (!runner.hasHandlers("tool_call")) { return undefined; }      // <-- no extension => no gate
  try {
    return await runner.emitToolCall({ type: "tool_call", toolName: toolCall.name, toolCallId: toolCall.id, input: args as Record<string, unknown> });
  } catch (err) {
    if (err instanceof Error) throw err;
    throw new Error(`Extension failed, blocking execution: ${String(err)}`);
  }
};
```

If no extension registered a `tool_call` handler, `beforeToolCall` returns `undefined` and the call runs. The default pi install has none — so a fresh pi runs every model tool call with no approval and no policy check.

### 2.3 Execution: `bash` goes straight to `spawn`, no sandbox

`createBashToolDefinition.execute` resolves the command (optionally prefixed), then calls the default local backend `createLocalBashOperations.exec`, which spawns a shell child (`packages/coding-agent/src/core/tools/bash.ts:282-404`, backend at `:66-126`):

```ts
const child = spawn(shell, [...args, command], {
  cwd,
  detached: process.platform !== "win32",
  env: env ?? getShellEnv(),
  stdio: ["ignore", "pipe", "pipe"],
  windowsHide: true,
});
```

No sandbox wrapper, no command inspection, no network/filesystem restriction. `shell`/`args` come from `getShellConfig` (the user's shell); `env` is the full `getShellEnv()`. The only safety-adjacent behavior is operational, not security: an optional `timeout` that kills the process tree (`bash.ts:95-100`), abort handling that kills the tree (`bash.ts:89-91, 104-108`), and output truncation (`bash.ts:341-368`). A pluggable `BashOperations.exec` / `spawnHook` exists so an *extension* could redirect execution to a container or SSH host (`bash.ts:40-57, 128-150`), but the shipped default is raw local spawn.

### 2.4 Execution: `write`/`edit`/`read` resolve any path and operate

`write.execute` resolves the model's `path` against cwd and writes, creating parent dirs, with no containment check (`packages/coding-agent/src/core/tools/write.ts:201-218`):

```ts
const absolutePath = resolveToCwd(path, cwd);
const dir = dirname(absolutePath);
return withFileMutationQueue(absolutePath, async () => {
  ...
  await ops.mkdir(dir);
  await ops.writeFile(absolutePath, content);
  ...
});
```

`resolveToCwd` → `resolvePath` simply joins-or-absolutizes and `path.resolve`s — so `/etc/hosts`, `~/.ssh/authorized_keys`, or `../../secret` all resolve to valid targets and are written (`packages/coding-agent/src/utils/paths.ts:81-85`). `edit` is the same: `const absolutePath = resolveToCwd(path, cwd)` then read/access/write with no containment (`edit.ts:312-351`; `defaultEditOperations` at `edit.ts:82-86`). `read` likewise resolves with `resolveToCwd`/`resolveReadPathAsync` and reads whatever resolves (`read.ts:15, 127`). `withFileMutationQueue` (`tools/file-mutation-queue.ts:32-61`) serializes writes to the *same* canonical path — a concurrency-correctness device (it even `realpath`s to dedupe symlinks), not a security boundary.

## 3. Subsystem findings

### 3.1 Approval model — none built in; extension-synthesizable, lossy in non-interactive modes

- **No native approval at all.** No policy matrix, no per-tool/per-arg rules, no "always allow" memory, no escalation. The loop's only branch is the binary `beforeToolCall` → `{ block?: boolean; reason?: string }` (`packages/agent/src/types.ts:55-58`, `packages/coding-agent/src/core/extensions/types.ts:984-988`). There is nothing to *remember* — a hook re-runs every call and must re-decide.
- **Extensions can build a confirm-gate.** The documented pattern is `on("tool_call")` + `ctx.ui.confirm(...)` returning `{ block: true, reason }` (`packages/coding-agent/docs/extensions.md:69-72, 690-712`). Two of the doc's example extensions are exactly this: `permission-gate.ts` ("Block dangerous commands", `on("tool_call")` + `ui.confirm`) and `protected-paths.ts` ("Block writes to specific paths", `on("tool_call")`) (`packages/coding-agent/docs/extensions.md:2541-2542`). These are *examples for users to write*, not shipped code.
- **`ui.confirm` degrades silently outside interactive mode.** The mode table: Interactive = full TUI; RPC = host handles UI; **JSON mode and Print (`-p`) = "No-op" / "can't prompt"** (`packages/coding-agent/docs/extensions.md:2506-2513`). So an extension approval gate that calls `ui.confirm` will get a falsy/auto result in `-p` and `--mode json`. In RPC mode the host *can* answer via `extension_ui_request`/`extension_ui_response` (`packages/coding-agent/src/modes/rpc/rpc-mode.ts:141-143`, `rpc-types.ts:215, 257`), but that is plumbing for an extension dialog, not a built-in tool-approval protocol.
- **Ordering of `tool_call` handlers across extensions:** runner iterates extensions in order, and the *first* handler that returns `{ block: true }` short-circuits and blocks (`packages/coding-agent/src/core/extensions/runner.ts:806-827`). Non-blocking handlers may also mutate `event.input` in place to patch arguments (no re-validation afterward — `extensions/types.ts:816-820`, doc `extensions.md:684-688`).

### 3.2 OS sandboxing — absent

- No mechanism on any platform. Searching the agent + coding-agent source for `sandbox|seatbelt|landlock|seccomp|bubblewrap|sandbox-exec|firejail` yields only: `bun/restore-sandbox-env.ts` (recovering env *inside* an external sandbox), a comment in `interactive-mode.ts:842` ("If we couldn't query tmux (timeout, sandbox, etc.)"), and the import of `restoreSandboxEnv` in `bun/cli.ts:9`. None create or enforce a sandbox.
- `bash` execution is unconfined local spawn (§2.3). No filesystem-scope restriction, no network-egress restriction, no exec restriction. The process inherits the operator's full environment via `getShellEnv()` (`bash.ts:82`).
- The docs point users at *external* isolation: build workflows "as extensions or packages, or use external tools such as containers and tmux" (`packages/coding-agent/docs/usage.md:277`). Containerization is the user's job, achievable via the `BashOperations`/`spawnHook` seams (`bash.ts:40-57, 128-150`) — pi provides the hook, not the sandbox.

### 3.3 Path & workspace policy — cwd-relative resolution, no containment

- "In-workspace" is **not enforced** for tool side effects. `resolveToCwd(path, cwd)` (`tools/path-utils.ts:48-50`) → `resolvePath` (`utils/paths.ts:81-85`) resolves relative paths against cwd and absolute paths as-is. `write`/`edit`/`read` then act on the result with no check that it's under cwd.
- A containment helper *exists but is not used as a guard*: `getCwdRelativePath` computes whether a path is inside cwd (rejecting `..`-escapes and absolutes) and returns `undefined` if outside (`utils/paths.ts:87-96`). Its only callers are *display* formatters — `formatPathRelativeToCwdOrAbsolute` (`paths.ts:98-101`) used to pretty-print tool titles — not access control. So pi *knows how* to detect an escape but never blocks on it.
- **Symlink escapes are not handled for access control.** `file-mutation-queue.ts` calls `realpath` (`file-mutation-queue.ts:17-26`) solely to key the per-file write lock; the resolved real path is not compared against a workspace root. `canonicalizePath` (`utils/paths.ts:28-34`) exists but is not used to confine writes. A symlink inside cwd pointing outside is followed normally.
- `path-utils.ts` spends its effort on macOS filename *recovery* (NFD normalization, narrow-no-break-space before AM/PM, curly-quote variants) to *find* files the model named imprecisely (`tools/path-utils.ts:52-118`) — i.e., making path resolution more permissive, the opposite of a containment policy.

### 3.4 Destructive-action detection — absent (lives only in doc examples)

- No `rm -rf` / `sudo` / force-push / `git reset --hard` pattern matching in any tool. `bash` does not inspect the command string at all beyond optional prefixing (`bash.ts:289`).
- The *only* occurrences of such patterns are documentation showing an extension a user would author: `event.input.command.includes("rm -rf")` → `{ block: true, reason: "Dangerous command" }` (`packages/coding-agent/docs/extensions.md:69-72, 703-705`), and the listed example extensions `permission-gate.ts` / `protected-paths.ts` (`extensions.md:2541-2542`). Nothing ships.

### 3.5 Prompt-injection / tool-output trust boundary — none

- There is no trust boundary on tool output. Tool results (including full `bash` stdout/stderr and `read` file contents) flow back into the transcript and become LLM context verbatim. Output is *truncated for size* — `bash` to last `DEFAULT_MAX_LINES`/`DEFAULT_MAX_BYTES` with overflow spilled to a temp file (`bash.ts:341-368`, `tools/truncate.ts`) — but never *sanitized*, tagged as untrusted, or fenced against injected instructions.
- The afterToolCall / `tool_result` hook could in principle scrub or re-tag output (`agent-session.ts:418-443`, `extensions/types.ts:880-1002`), but again no shipped extension does, and it runs *after* the side effect. Pi has no equivalent of "untrusted content" markers or a content firewall.

### 3.6 Fail-closed behavior — mostly fail-open, with one fail-safe

- **Unknown tool:** fail-closed in the trivial sense — `prepareToolCall` returns an error result "Tool X not found" rather than executing anything (`agent-loop.ts:570-576`). But that's because there's nothing to run, not a policy decision.
- **Unmatched rule:** N/A — there is no rule engine. The default is **fail-open**: absent a `beforeToolCall` hook, every call executes.
- **`tool_call` handler error:** **fail-safe (blocks).** A throw from the extension hook is treated as a block — the coding-agent wrapper rethrows (`agent-session.ts:410-415`), and the doc states "`tool_call` errors block the tool (fail-safe)" (`packages/coding-agent/docs/extensions.md:2503`). So an extension that *throws* denies the call; one that *returns nothing* allows it.
- **Schema-validation error:** the call is blocked with a formatted validation error returned to the model so it can retry (`validation.ts:315-323`; loop catches and converts to an error result, `agent-loop.ts:619-624`). But on AJV/TypeBox *compile* failure in restricted runtimes, validation is skipped and the call proceeds unvalidated (`packages/ai/CHANGELOG.md` Cloudflare-Workers note; `validation.ts:66-75` swallows sub-schema compile errors). That path is fail-open.

### 3.7 Bypass / yolo modes — N/A (nothing to bypass)

- There is no danger/yolo flag because there is no guard to disable. No `--dangerously-skip-permissions`, no `bypassPermissions`, no `--yolo`. Zero hits in coding-agent source for any of these.

### 3.8 Where the guard sits + how a blocked call surfaces

- Guard position: between schema validation and `tool.execute`, inside `prepareToolCall` (`agent-loop.ts:581-605`) — i.e., *pre-dispatch*, per tool, sequentially even in parallel-execution mode (the doc notes sibling calls are "preflighted sequentially, then executed concurrently", `extensions.md:680`).
- A blocked call becomes an `isError` tool result with `reason` as the text (`agent-loop.ts:598-604`, `createErrorToolResult` at `:710-714`), appended like any other result and re-fed to the model. There is **no pending/awaiting state**: the loop has no notion of "paused for approval." An extension's `ui.confirm` blocks the hook's promise synchronously within the turn; while it's open the loop is simply awaiting that promise, not in a distinct UI approval state machine.

### 3.9 Managed binaries / PATH trust

- `grep`/`find` shell out to `rg`/`fd` resolved by `getToolPath`: local managed-bin dir first, else **the first matching name found on the system PATH** (`packages/coding-agent/src/utils/tools-manager.ts:85-104`), else download a pinned release from GitHub (`tools-manager.ts:106-129`; gated by `PI_OFFLINE`, `:14-18`). So pi trusts whatever `rg`/`fd` is on PATH — a PATH-poisoning vector, though low-impact since they're invoked with arg arrays, not shell strings (`grep.ts:214-220`, `find.ts:216, 250`), so no command injection through the search pattern. Auth credential file is written `0600` (`packages/coding-agent/docs/providers.md`).

## 4. Non-obvious design choices

- **Guardrails are a userland concern by explicit design.** Pi's stance is that the harness stays minimal and the operator composes safety from extensions and external isolation (`usage.md:277`; `extensions.md` Security note). The `beforeToolCall`/`tool_call` seam is real and sufficient to build a gate, but pi deliberately ships none — so "out of the box" pi is maximally permissive.
- **`afterToolCall` can rewrite but not prevent.** The only post-execution hook fires after side effects (`agent-loop.ts:665-708`). Useful for redaction/annotation, useless for prevention. Any real guard must live in `beforeToolCall`.
- **Pluggable execution as the isolation story.** Rather than an in-process sandbox, pi exposes `BashOperations`/`spawnHook` (`bash.ts:40-57, 128-150`) and `WriteOperations`/`EditOperations` (`write.ts:25-35`, `edit.ts:73-86`) so an extension can redirect *where* side effects land (container, remote host). Isolation is delegated, not implemented.
- **The containment check exists but is wired only to display.** `getCwdRelativePath` (`paths.ts:87-96`) could be a workspace guard; it's used purely to format tool titles. This is the clearest "the primitive is here, the policy is not" signal in the codebase.
- **Validation is intentionally lenient.** `validateToolArguments` coerces aggressively (string→number, "true"→bool, JSON-string→array) and falls back to running unvalidated if schema compilation fails (`validation.ts:77-149, 297-309`). Optimized for model-tolerance, not for rejecting malformed/oversized input.

## 5. Under-developed or risky areas (from a single-operator-safety lens)

- **Zero default protection.** A default pi run will `rm -rf`, write `~/.ssh/authorized_keys`, or `curl | sh` whatever the model emits, with no prompt. For nav this is the entire gap to fill.
- **No path containment even when detectable.** Writes/reads/edits to absolute paths and `..`-escapes outside cwd succeed silently; the symlink real-path is computed only for lock-keying, not confinement.
- **Approval gates degrade in headless modes.** Even a user-installed `ui.confirm` gate is a no-op under `-p` / `--mode json` (`extensions.md:2506-2513`), so an extension-based "are you sure?" provides *no* protection in exactly the automated contexts where unattended damage is most likely.
- **Validation fail-open on compile error.** The restricted-runtime fallback runs tool calls without schema validation (`validation.ts` + CHANGELOG). Narrow, but it's a fail-open path in the one mandatory check.
- **Tool output is fully trusted.** No injection boundary; `read`/`bash` output re-enters context verbatim, so a malicious file or command output can carry instructions straight to the model.
- **PATH trust for `rg`/`fd`.** Resolves the first PATH match before downloading (`tools-manager.ts:95-101`); mitigated by arg-array invocation but still trusts ambient binaries.

## 6. Implications for nav

**What pi gives nav for free:**
- A clean, correctly-placed interception seam. `beforeToolCall` sits exactly where nav's guard belongs — after schema validation, before `tool.execute`, per-tool, pre-dispatch (`agent-loop.ts:581-605`). nav's approval flow (APR-01) can occupy this slot directly instead of inventing a new one. The `{ block, reason }` contract and the "error tool result fed back to the model" surfacing (`agent-loop.ts:598-604`) are a reasonable shape to reuse for *deny*.
- Pluggable execution operations (`BashOperations`/`spawnHook`, `WriteOperations`, `EditOperations`) are a ready-made injection point for sandboxed/containerized execution backends without touching tool logic.
- `getCwdRelativePath` (`paths.ts:87-96`) is a working in-workspace predicate nav can promote from display-only to an enforced write/edit guard, plus `canonicalizePath` (`paths.ts:28-34`) for symlink-aware containment.
- The aggressive macOS path-recovery and output-truncation logic are safe to keep as-is — orthogonal to guardrails.

**Gaps nav must fill itself (APR-01 / APR-03):**
- A **first-class approval flow** with a real pending/awaiting-approval state in the loop and UI — pi has none; a blocked call is just an error result, and there's no "remember this decision" / "always allow" memory. nav should make approval a distinct state, not an error.
- A **policy/guardrails engine** (per-tool × arg-class rules, allow/deny/ask, remembered decisions, fail-closed defaults) — pi's only "policy" is a single binary hook with no persistence.
- **Path/workspace enforcement** on `write`/`edit`/`read` — promote `getCwdRelativePath` to a hard guard, add a writable-root allowlist, and reject symlink/`..` escapes using `realpath` comparison (pi computes realpath but never compares it).
- **Destructive-action detection** for `bash` — pi has none; nav needs at least pattern-based detection (`rm -rf`, force-push, `sudo`, fork bombs) feeding the approval flow.
- **OS sandboxing** — pi has nothing; this is where the codex dive (seatbelt/landlock) is the better template. nav can use pi's `BashOperations` seam as the wiring point but must supply the actual confinement.
- **A tool-output trust boundary** — pi trusts output verbatim; nav should tag tool output as untrusted before re-entry.
- **Fail-closed defaults** — pi is fail-open absent a hook; nav's default should be ask/deny on unknown tool or policy-eval error.

**Patterns worth porting vs. skipping:**
- *Port:* the `beforeToolCall` choke-point placement and the pluggable-operations seam (clean, minimal, correct location). The `tool_call`-error-blocks fail-safe semantic (`extensions.md:2503`) is a good default for nav's policy-evaluation errors.
- *Skip:* the "guardrails are entirely the user's extension responsibility" stance — for a single-operator local agent that defaults to unconfined execution, that's the gap nav is closing. Also skip the lenient validation fall-open and the display-only containment; nav should make both enforcing.
