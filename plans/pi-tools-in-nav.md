# Pi tools in nav

Status: research / planning. No implementation in this document.

Related: [architecture.md](./architecture.md), [session-storage.md](./session-storage.md), [model-provider-settings.md](./model-provider-settings.md).

Sources studied:

- Pi coding-agent tools: `/Users/season/Personal/pi/packages/coding-agent/src/core/tools/`
- Pi tool registry and presets: `.../tools/index.ts`
- Pi extension API: `.../core/extensions/types.ts`, `loader.ts`
- Pi managed binaries: `.../utils/tools-manager.ts`
- Nav harness placeholders: `crates/nav-harness/src/tools/`, `workspace/`, `guardrails/`, `integrations/mcp.rs`
- Nav wire contract: `crates/nav-protocol/src/events.rs`, `rpc.rs`

---

## Summary

Pi exposes **seven built-in LLM tools** for coding agents: `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`. They are native function-calling tools (TypeBox schemas → provider `tools` array), not MCP. Additional tools come from **extensions** (`pi.registerTool`) or SDK `customTools`.

Nav already defines a **frontend-agnostic tool lifecycle** on the wire (`tool.call_*`, `tool.approval_requested`, `tool.approve` / `tool.reject`) and can parse provider tool-call streams into harness events. Execution, registry, workspace ops, and the agent loop are **not implemented yet**.

The right shape for nav is:

1. Implement each Pi builtin in **`nav-harness`** (`tools/` + `workspace/`), behind a typed **`ToolRegistry`**.
2. Run tools from an **`agents/` loop** that persists canonical `ToolCall` / `ToolResult` parts per [session-storage.md](./session-storage.md).
3. Gate risky tools through **`guardrails/`** and surface approvals via existing protocol events (TUI calls `tool.approve` / `tool.reject`).
4. Keep **`nav-server`** thin: map harness events → `BackendEvent`, route RPC only.

Pi’s **extension tool** model maps to nav’s planned **skills** + **MCP integrations**, not to a second parallel tool system in the protocol.

---

## Pi tool architecture (reference)

```text
ToolDefinition (schema + execute + optional TUI render)
    → wrapToolDefinition → AgentTool
    → AgentSession._toolRegistry (builtins + extensions + customTools)
    → active subset in agent.state.tools
    → pi-ai Context.tools → model function calling
    → agent-loop: validateToolArguments → execute → tool_result message
```

| Concern | Pi location |
| --- | --- |
| Schemas | Per-tool `*Schema` in `core/tools/*.ts` (TypeBox) |
| Default active set | `read`, `bash`, `edit`, `write` (`sdk.ts`, `agent-session.ts`) |
| Read-only preset | `read`, `grep`, `find`, `ls` |
| File mutation ordering | `file-mutation-queue.ts` serializes `edit`/`write` per realpath |
| Search binaries | `tools-manager.ts` ensures `rg` / `fd` (system PATH or `~/.pi/agent/bin/`) |
| Remote/sandbox hooks | `*Operations` interfaces (`ReadOperations`, `BashOperations`, …) |
| Extension tools | `registerTool(ToolDefinition)`; merged in `_refreshToolRegistry()` |
| Hooks | `beforeToolCall` / `afterToolCall` on Agent + extension `tool_call` / `tool_result` events |

Pi has **no built-in human approval RPC** for tools; extensions can block in `beforeToolCall`. Nav’s explicit approval flow is a deliberate product difference.

---

## Nav target architecture (where tools live)

| Layer | Responsibility for Pi-like tools |
| --- | --- |
| `nav-harness::tools` | `ToolRegistry`, schemas (JSON Schema for encoders), `execute`, truncation, result shaping |
| `nav-harness::workspace` | cwd resolution, path policy, `tokio::fs`, process spawn, optional git metadata |
| `nav-harness::guardrails` | hook runner, normalized tool-call context, allow/deny/approval decisions, result redaction |
| `nav-harness::agents` | loop: model turn → tool calls → execute → append turns → repeat |
| `nav-harness::sessions` | durable tool call/result parts, approval state |
| `nav-harness::events` | internal `HarnessEvent` (already has tool stream variants) |
| `nav-harness::integrations::mcp` | adapt external MCP tools into same registry |
| `nav-server` | RPC handlers, SSE projection (`event_mapping.rs`) |
| `nav-protocol` | stable event/RPC names (extend only if clients need new capabilities) |
| `tui` | render tool UI; approve/reject; no execution |

```text
Frontend ──RPC/SSE──► nav-server ──► nav-harness::agents
                                      ├─ ToolRegistry (read, bash, …)
                                      ├─ GuardrailEngine
                                      ├─ SessionStore (canonical parts)
                                      └─ models::encode (tools[] in provider request)
```

---

## Cross-cutting design (all tools)

These apply to every Pi builtin when porting to nav.

### 1. `NavTool` trait and registry

Replace the empty `ToolRegistry` stub with:

- **Registration**: name, description, JSON Schema parameters, risk class (`read` | `mutate` | `exec` | `search`).
- **Execution**: `async fn execute(ctx, args, cancel_token) -> ToolResult` where `ToolResult` is text and/or image blocks for the encoder.
- **Active set**: session settings mirror Pi presets (`coding`, `readonly`, explicit allowlist).
- **Provider exposure**: encoder builds OpenAI/Anthropic tool definitions from the same schema registry.

Suggested layout:

```text
crates/nav-harness/src/tools/
  mod.rs           # ToolRegistry, NavTool trait, presets
  schema.rs        # shared JSON Schema helpers
  truncation.rs    # port Pi DEFAULT_MAX_BYTES / MAX_LINES behavior
  path.rs          # cwd-relative resolve, deny `..` escapes, optional workspace roots
  read.rs
  write.rs
  edit.rs
  bash.rs
  grep.rs
  find.rs
  ls.rs
```

### 2. Session cwd and path policy

Pi tools take a session **`cwd`** at factory time (`createReadTool(cwd, options)`).

Nav should set `cwd` from `session.create` params (already has optional `cwd` in protocol) and enforce:

- Resolve relative paths against session cwd.
- Reject paths outside allowed roots (workspace + explicit grants from guardrails).
- Use the same rules for `read`, `write`, `edit`, `grep`, `find`, `ls`, and bash’s implicit cwd.

Implement in `nav-harness::workspace::path` (new module), not in each tool ad hoc.

### 3. Truncation and large output

Pi centralizes truncation in `truncate.ts` (head/tail, byte and line limits). Bash spills huge stdout to a temp file path in the tool result.

Nav should:

- Add `tools/truncation.rs` with matching defaults and user-visible “truncated” markers in tool results.
- For `bash`, stream chunks to harness events if we want TUI progress (`tool.call_delta` is today used for **argument** streaming from the model; consider a separate `tool.output_delta` later, or reuse `message.delta` for live shell output — **defer** until TUI needs it; v1 can return one blob like Pi).

### 4. File mutation queue

Pi serializes `edit` and `write` per file via `withFileMutationQueue`.

Nav should use a **`tokio::sync::Mutex` keyed by canonical path** (after `canonicalize` or resolved path) inside `workspace` or `tools/file_queue.rs`, called from `write` and `edit` only.

### 5. Managed binaries (`rg`, `fd`)

Pi downloads `rg` and `fd` to `~/.pi/agent/bin/` when missing.

Nav options (pick one for v1):

| Option | Pros | Cons |
| --- | --- | --- |
| A. Require system `rg`/`fd` in PATH | Simple Rust | Worse onboarding |
| B. Vendor or download to `~/.nav/bin/` | Parity with Pi | Maintenance, platform matrix |
| C. Pure Rust fallback for `grep`/`find` | No external deps | Slower, semantic drift vs `rg` |

**Recommendation:** A for first milestone; B as follow-up if dogfooding hurts. Document in README like Pi’s tools manager.

### 6. Approvals (nav advantage over Pi)

Pi: extensions may throw in `beforeToolCall` to block.

Nav: first-class protocol:

1. Agent loop builds `ToolCallContext` and runs guardrail hooks before execution.
2. A hook returns `RequestConfirmation { reason, summary }`.
3. Emit `tool.approval_requested` with `approval_id`, pause run.
4. Frontend calls `tool.approve` or `tool.reject` (wire exists; **server handlers TODO**).
5. On approve, resume the same call through the scripted approval channel; on reject, append error result and continue or fail run per policy.

Suggested default policy (align with Pi’s default active tools):

| Tool | Default approval |
| --- | --- |
| `read`, `grep`, `find`, `ls` | auto-allow |
| `edit`, `write` | auto-allow in “trusted cwd”; optional confirm for paths outside workspace |
| `bash` | always approve (or pattern-based allowlist: `cargo test`, `git status`, …) |

### 7. Observability and `file.changed`

After successful `write`/`edit`, emit harness `file.changed` → protocol `file.changed` so frontends can refresh tree/diff views. Pi does this implicitly via TUI watching the editor; nav should make it explicit on the wire.

### 8. Extension / custom tools (Pi parity)

Pi `registerTool` → nav:

- **Skills** (`nav-harness::skills`): packaged instructions + optional tool adapters.
- **MCP** (`integrations/mcp.rs`): each MCP tool becomes a `NavTool` wrapper (name namespaced `mcp:<server>/<tool>`).
- **Dynamic registration**: not in v1 protocol; reload via backend restart or future `initialize` capability flags.

Do not add Pi-style `~/.nav/tools/*.ts` unless we later embed a script runtime; Rust-first keeps the learning goal clear.

---

## Per-tool mapping

### `read`

**Pi behavior** (`read.ts`):

- Params: `path` (required), `offset?`, `limit?` (1-based lines).
- Text files: line-numbered output, truncation by bytes/lines.
- Images: detect MIME, optional resize (default max 2000×2000), return image content blocks to the model.
- `ReadOperations` for SSH/remote override.
- TUI: syntax highlight, compact mode for `AGENTS.md` / skills.

**Nav implementation plan:**

| Piece | Location | Notes |
| --- | --- | --- |
| Path resolve + access check | `workspace::path` + `tokio::fs::metadata` | Deny directories unless we add `read` directory mode (Pi reads files only) |
| Line range | `tools/read.rs` | Match Pi offset/limit semantics |
| Truncation | `tools/truncation.rs` | Same defaults as Pi |
| Images | `tools/read.rs` + `models::encode` | Return base64 image parts in canonical `ToolResult`; encoder maps to provider image blocks |
| Remote | Defer | `ReadOperations` equivalent = trait object on `Workspace` later (SSH story) |
| Wire | Existing tool events + `file.changed` N/A | Tool result only |

**Schema (LLM):** keep Pi field names (`path`, `offset`, `limit`) for prompt compatibility.

---

### `bash`

**Pi behavior** (`bash.ts`):

- Params: `command` (required), `timeout?` (seconds).
- Spawns shell with cwd, streams stdout/stderr, accumulates output, truncates or writes temp file for overflow.
- `BashOperations` / `spawnHook` for sandbox/SSH.

**Nav implementation plan:**

| Piece | Location | Notes |
| --- | --- | --- |
| Spawn | `workspace::shell` | `tokio::process::Command`, session cwd, inherit minimal env (document whitelist) |
| Timeout | `tools/bash.rs` | `tokio::time::timeout` |
| Output cap | `tools/truncation.rs` | Match Pi; optional spill to temp under `{data_dir}/tool-output/` |
| Approval | `guardrails` | Bash hook returns `RequestConfirmation`; APR-02 stores pending command in approval payload |
| Cancellation | `run.cancel` | Kill child process group when run cancelled |
| User shell (`!` in Pi) | Defer or `tui` local | Pi’s `user_bash` is not an LLM tool; nav can keep as frontend-only exec or later RPC `shell.exec` |

**Security:** This is the highest-risk tool. Guardrails must parse command argv, block `curl | sh`, forced `rm -rf`, etc., before approve UI.

---

### `edit`

**Pi behavior** (`edit.ts`):

- Params: `path`, `edits[]` with `oldText` / `newText` (non-overlapping replacements on original file); legacy single `oldText`/`newText` shim.
- Uses diff logic (`edit-diff.ts`); fails if unique match not found.
- Queued via `withFileMutationQueue`.

**Nav implementation plan:**

| Piece | Location | Notes |
| --- | --- | --- |
| Read-modify-write | `tools/edit.rs` | Read full file as UTF-8 (or bail on binary) |
| Patch application | `tools/edit.rs` or small `diff` crate | Port Pi’s “unique match” rules; don’t use blind patch hunks |
| Queue | `tools/file_queue.rs` | Per-path serialization |
| Validation | Before write | If zero or multiple matches for an `oldText`, return structured error to model |
| `file.changed` | After success | Emit with path + change kind |
| Approval | Policy | Auto for in-workspace paths |

Consider **`similar` crate** or `diffy` only for user-facing error messages, not fuzzy apply.

---

### `write`

**Pi behavior** (`write.ts`):

- Params: `path`, `content` (full file).
- Creates parent dirs; overwrites; mutation queue.

**Nav implementation plan:**

| Piece | Location | Notes |
| --- | --- | --- |
| Write | `workspace::fs` + `tools/write.rs` | `tokio::fs::create_dir_all` + `write` |
| Queue | Same as `edit` | |
| Binary | v1 text-only | Reject or base64-decode explicit later |
| `file.changed` | Yes | |

---

### `grep`

**Pi behavior** (`grep.ts`):

- Params: `pattern`, `path?`, `glob?`, `ignoreCase?`, `literal?`, `context?`, `limit?` (default 100).
- Spawns `rg` with structured args.

**Nav implementation plan:**

| Piece | Location | Notes |
| --- | --- | --- |
| Backend | `tools/grep.rs` | `std::process::Command::new("rg")` with `--json` or line output |
| Path scope | `workspace::path` | Default search cwd |
| Limits | Enforce `limit` + truncation on total output bytes |
| Fallback | Later | Walkdir + regex if no `rg` |
| Approval | Auto | Read-only |

Map Pi’s `literal` → `rg -F`, `ignoreCase` → `-i`, `context` → `-C`.

---

### `find`

**Pi behavior** (`find.ts`):

- Params: `pattern` (glob), `path?`, `limit?` (default 1000).
- Spawns `fd`.

**Nav implementation plan:**

| Piece | Location | Notes |
| --- | --- | --- |
| Backend | `tools/find.rs` | `fd` with glob; respect limit |
| Fallback | Later | `globwalk` crate |
| Approval | Auto | Read-only |

---

### `ls`

**Pi behavior** (`ls.ts`):

- Params: `path?`, `limit?` (default 500).
- Native `readdir` + metadata; formatted listing.

**Nav implementation plan:**

| Piece | Location | Notes |
| --- | --- | --- |
| List | `tools/ls.rs` | `tokio::fs::read_dir`, sort, include dir/file/size/mtime |
| Limit | Truncate listing | Match Pi |
| Approval | Auto | Read-only |

---

## Supporting assets (not LLM tools)

| Pi asset | Nav equivalent |
| --- | --- |
| `tools-manager` (`fd`, `rg`) | `nav-backend` install script or `workspace::binaries` module |
| `file-mutation-queue` | `tools/file_queue.rs` |
| `path-utils` | `workspace/path.rs` |
| `truncate.ts` | `tools/truncation.rs` |
| TUI tool renderers | `tui/internal/ui` tool cards from SSE events (later) |

---

## Agent loop integration (required for any tool to work)

Today `nav-server` streams a **single model completion** without executing tools. To use Pi’s tools, nav needs:

```text
session.sendMessage
  → Run started
  → loop:
       encode(session turns + tool schemas) → provider
       if assistant has tool_calls:
         for each call:
           emit tool.call_requested (optional)
           guardrails.before_tool_call → RequestConfirmation? → tool.approval_requested → wait RPC
           emit tool.call_started
           registry.execute
           emit tool.call_completed
           append ToolResult part to session store
         continue loop
       else:
         append assistant text part; complete run
```

Harness already maps **provider** tool-call streaming (`events/mod.rs`, `openai_completions.rs`). Next step is **closing the loop** with registry execution and canonical persistence per [session-storage.md](./session-storage.md).

Tool schemas must be included in `models::encode` for each `ApiKind` (OpenAI `tools`, Anthropic `tool_use`, etc.).

---

## Protocol and TUI gaps (tool-related)

| Item | Status | Action |
| --- | --- | --- |
| `tool.call_requested` / `started` / `completed` | Defined | Emit from harness when loop runs |
| `tool.approval_requested` | Defined | Emit when guardrail requires |
| `tool.approve` / `tool.reject` | Defined, not routed in server | Implement in `nav-server` → harness |
| `initialize` + `toolApprovals` capability | Fixture only | Set `true` when approvals work |
| TUI tool rendering | Minimal | Subscribe to tool events; approval modal |
| `tool.call_delta` | Used for **model arg streaming** | Keep separate from shell stdout |

---

## Suggested implementation phases

### Phase 1 — Registry + read-only tools

- `NavTool` trait + `ToolRegistry` presets (`coding`, `readonly`).
- `read`, `ls`, `grep`, `find` (system `rg`/`fd`).
- Path policy + truncation.
- Wire tools into encoder; **manual** test via harness integration test (no full loop yet if needed).

### Phase 2 — Mutating tools + queue

- `write`, `edit` + file mutation queue + `file.changed` events.
- Session store writes `ToolCall` / `ToolResult` parts.

### Phase 3 — Agent loop + bash

- Full loop in `agents/`.
- `bash` with approval flow and `run.cancel` propagation.
- Server: `tool.approve` / `tool.reject`, `session.close`.

### Phase 4 — Parity extras

- Binary manager for `rg`/`fd`.
- MCP → registry adapters.
- Skills-based custom tools.
- Remote `*Operations` hooks (SSH) if still desired.

---

## Intentional differences from Pi

| Topic | Pi | Nav (planned) |
| --- | --- | --- |
| Tool transport | In-process TS only | Rust harness; SSE for all frontends |
| Approvals | Extension hooks | Protocol RPC + guardrail hooks |
| TUI rendering | Built into `ToolDefinition` | Frontend renders from events; harness returns plain results |
| Default tools | 4 active (`read`,`bash`,`edit`,`write`) | Same default reasonable for coding agent |
| MCP | Via extensions only | First-class `integrations/mcp` into same registry |
| Config | Pi `models.json` | Already compatible per README; tool allowlist in session settings |

---

## Open questions

1. **Bash approval UX**: always confirm vs pattern allowlist stored per workspace?
2. **Image read**: required for v1 or defer until a vision model is selected?
3. **`grep`/`find` without ripgrep/fd**: ship fallbacks or hard-error with install instructions?
4. **Tool output streaming**: does TUI need live bash output, or post-hoc result only?
5. **Edit semantics**: strict Pi exact-match only, or allow optional fuzzy patch later?

---

## Checklist: Pi built-in → nav crate mapping

| Pi tool | Primary nav module | Depends on |
| --- | --- | --- |
| `read` | `tools/read.rs` | `workspace/path`, `truncation`, encoder image parts |
| `write` | `tools/write.rs` | `file_queue`, `workspace/fs`, guardrails |
| `edit` | `tools/edit.rs` | `file_queue`, `workspace/fs`, guardrails |
| `bash` | `tools/bash.rs` | `workspace/shell`, guardrails, approvals, cancel |
| `grep` | `tools/grep.rs` | `rg` binary, `workspace/path` |
| `find` | `tools/find.rs` | `fd` binary, `workspace/path` |
| `ls` | `tools/ls.rs` | `workspace/path` |
| (extension tools) | `skills/`, `integrations/mcp` | registry plugin API |
| (registry) | `tools/mod.rs` | `agents/`, `models/encode` |
| (guardrails) | `guardrails/` | sessions approvals |
| (persistence) | `sessions/` | SQLite parts per session-storage plan |
| (wire) | `nav-server/event_mapping` | existing `BackendEvent` variants |

This document should be updated when the first tool lands in `nav-harness` or when protocol capabilities change.
