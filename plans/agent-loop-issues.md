# Agent loop + tools — GitHub issue carve-up

Status: brainstorm (rev 2 — incorporates Codex review). Issues not yet filed. Sequel to [pi-tools-in-nav.md](./pi-tools-in-nav.md); informed by Geoffrey Huntley's "How to build a coding agent" (https://ghuntley.com/agent/).

## Context

Today nav can stream one model completion per `session.sendMessage` and has no agent loop, no multi-turn state, no tools, and no approval routing. Almost every `nav-harness` module besides `models/` and `events/` is a 4-line stub. The Ink TUI just landed but its `NavEvent` and `HistoryMessage` types only model text/error payloads — there is no structure for tool calls, approvals, or file changes (`tui/src/backend/client.ts:46`, `tui/src/regions/history/types.ts:1`), and unknown events are dropped at `App.tsx:236`.

Huntley's prescription — read → list → bash → edit → grep, plus a harness prompt, all wrapped in a "300 lines in a loop" agent loop — maps directly onto the gap. The one place nav should diverge is approvals: `tool.approve` / `tool.reject` are already in the protocol with no handlers, and approvals are a stated product differentiator, so `bash` should land *with* the approval flow, not before it.

This document carves the work into scoped issues with sequence and a strong-model / weak-model label, following the AM-\* convention (architectural piece = strong, satellite work = weak).

## Key correction from Codex review

The first draft systematically under-counted TUI work. Tool, approval, cancel, and `file.changed` events all need new payload variants on `NavEvent`, new fields on `HistoryMessage`, and real App-state changes (composer is inert while busy, so approval input is not free). A new **INK-\*** track now sits alongside the backend tracks and *must* land in lockstep — surfacing a backend tool event with no TUI parser to read it is invisible to the user and untestable end-to-end.

Other shifts from rev 1:

- Mutation-safety work (path policy, write, file.changed) is **strong**, not weak.
- The first real tool is **bundled with encoding + dispatch** so encoding isn't reviewed against an empty registry.
- `APR-02` is **split** into RPC surface and run pause/resume state machine.
- `bash` (APR-03) is **strong** and absorbs cancellation (was APR-05).
- The TUI approval modal lands **before** any tool requires approval.
- Tool allowlist preset selection moves to **Phase 2**, not Phase 4 — it must gate availability the moment tools exist.
- `LOOP-05` and `APR-06` are deleted as standalone issues and folded into the implementing issues' acceptance criteria.

## Prefix proposal

- **LOOP-\*** — multi-turn state, agent loop scaffold, harness prompt
- **TOOL-\*** — registry, schema encoding, individual tools, file mutation queue
- **APR-\*** — guardrails, hook-confirmation RPC handlers, run pause/resume
- **INK-\*** — TUI event payload parsing, tool/approval/file cells, approval modal, client methods

Single `AG-*` umbrella was considered; four prefixes match how OR/PRT/RSM/TUI grouped past work and grep cleaner. `INK-*` is preferred over `TUI-*` because past closed `TUI-*` issues referenced the now-deleted Go TUI.

## Phase 1 — Loop foundation (no tools yet)

Goal: nav can hold a multi-turn conversation. Loop scaffold exists but always terminates after one model turn.

| # | Issue | Model | Scope |
|---|---|---|---|
| LOOP-01 | Multi-turn session message store | strong | In-memory `SessionStore` in `sessions/`; canonical `Turn { role, parts }`; `session.sendMessage` appends + reads prior turns. No disk yet. **Acceptance**: backend integration test asserts the outgoing provider request payload contains the prior assistant reply verbatim (test below the TUI boundary, not model semantics). |
| LOOP-02 | Assistant + Tool message roles | weak | Extend `ChatCompletionMessageRole` and `ChatCompletionRequestMessage` with `Assistant` and `Tool` variants. **Defines the OpenAI tool_call_id field and tool_result content encoding** that LOOP-03 / TOOL-03 will rely on. Depends on LOOP-01. |
| LOOP-03 | Agent loop scaffold in `agents/` | strong | `RunLoop::run()` that calls model → checks for tool_calls → terminates if none (no execution yet). Replaces direct `model_run::run_to_completion` call site. **Acceptance**: existing single-turn SSE contract preserved + two-turn integration test (assistant reply visible to second user message). Folds the deleted LOOP-05. |
| LOOP-04 | Harness system prompt module | weak | `context::system_prompt` builder: OS, cwd, date, conventions. Tool list renders from the registry — empty in this PR, populated automatically once TOOL-01 ships. `Clock` and `Cwd` are injectable for deterministic tests; cwd already flows through `session.create`, date does not yet. Inject as a `System` turn. |

## Phase 2 — Registry, first tool, allowlist, and TUI contract

Goal: prove schema → encode → call → execute → ToolResult → next turn on one real tool, with the TUI able to see what happened.

### Backend

| # | Issue | Model | Scope |
|---|---|---|---|
| TOOL-01 | `NavTool` trait + `ToolRegistry` + presets | strong | Trait, registration, `coding` / `readonly` presets, risk class. **Ships with an internal `echo` fake tool** so the API is exercised end-to-end before any real tool — eliminates abstract API-only review. |
| TOOL-02 | Tool allowlist in session settings | weak | Wire preset selection through `session.create` params; respects `coding` / `readonly`. **Lands with TOOL-01** so presets gate availability from the moment tools exist (the rev-1 mistake was burying this last). Extends the TUI client `createSession` signature too — pair with INK-04. |
| TOOL-03 | Path policy + truncation utilities | strong | `workspace::path` resolve + deny escapes (including symlinks pointing outside workspace) + `tools::truncation` with Pi-matching defaults. Acceptance covers `..` escapes, absolute paths outside cwd, and symlink traversal cases. Gates safety of read / bash / edit / write — relabeled strong. |
| TOOL-04 | OpenAI tool-schema encoding + `read` tool + dispatch | strong | Bundled because each is abstract apart: encoder builds `tools[]` from the registry, dispatch executes calls from the model, `read` proves the loop end-to-end on a safe tool. Uses TOOL-01 / TOOL-02 / TOOL-03 / LOOP-02 / LOOP-03. Text-only `read` (defer images). |
| TOOL-05 | `ls` tool | weak | Trivia after TOOL-04 — same path policy + dispatch path. Reasonable to fold into TOOL-04 if it grows; keep as separate weak issue for a clean PR. |
| TOOL-06 | Tool-call SSE event mapping | weak | Wire `tool.call_started` / `tool.call_completed` through `event_mapping.rs`. **Surfaces nothing user-visible without INK-01**; pair their merge. |

### TUI (INK-\* track)

| # | Issue | Model | Scope |
|---|---|---|---|
| INK-01 | Extend `NavEvent` + `HistoryMessage` with tool payloads | strong | Add discriminated variants for tool call, tool result, and (later-used) approval / file events; teach `applyEvent` to dispatch on type rather than `eventText(event)` fallthrough. Today unknown events become empty text and are ignored (`App.tsx:236`). Blocks INK-02. |
| INK-02 | `ToolCallCell` + `ToolResultCell` | strong | Real Ink components with parsed payload (tool name, args summary, result snippet, status). Not the rev-1 "one component, no special styling" — message model has no parsed tool fields today. Depends on INK-01. |

## Phase 3 — Approvals + `bash`

Goal: nav can run arbitrary commands with the approval gate that distinguishes it from Pi / Huntley's MVP. Approval UX lands *before* any tool requires approval.

### Backend

| # | Issue | Model | Scope |
|---|---|---|---|
| APR-01 | Tool guardrail hook contract | strong | Replace the policy-engine/table shape with a small deterministic hook runner. Tool dispatch builds a normalized `ToolCallContext`, first-party hooks return `Allow`, `Deny`, or `RequestConfirmation`, and after-hooks may redact/normalize results before they are returned to the model. Default with no hooks is allow after schema validation; non-interactive confirmation fails closed until APR-02a/APR-02b provide the approval channel. |
| APR-02a | Confirmation RPC handlers + protocol surface | strong | Backend wiring of `tool.approve` / `tool.reject` as answers to hook-requested confirmations; emit generic `tool.approval_requested` payloads with `approval_id`, tool name, reason, argument summary, and risk class. Server-side only; pause/resume remains APR-02b. |
| APR-02b | Run pause/resume state machine in harness | strong | Run lifecycle change: harness suspends the loop when a guardrail hook requests confirmation, resumes from the APR-02a approval/rejection signal, and treats run cancellation as a wakeup. Distinct PR from APR-02a because the failure modes don't overlap. |
| APR-03 | `bash` tool (strong, absorbs cancel) | strong | `workspace::shell`, `tokio::process`, cwd, env allowlist, timeout, output cap with temp-file spill, child process group kill on `run.cancel`. Approval gated via APR-01. Returns one blob in this PR — live streaming is APR-04. Acceptance includes the deleted APR-06 end-to-end approve + reject test as a merge gate. Absorbs the deleted APR-05 cancellation work. |
| APR-04 | Live `bash` output streaming (protocol + harness) | strong | New `tool.output_delta` SSE event variant (deliberately separate from `tool.call_delta`, which is model-arg streaming per the pi-tools doc). `bash` streams stdout/stderr chunks from the spawned process through harness events; final `tool.call_completed` still carries the truncated full result. Backpressure + chunk coalescing live here. Depends on APR-03. |

### TUI

| # | Issue | Model | Scope |
|---|---|---|---|
| INK-03 | Approval modal overlay | strong | Reuses `overlays/` shape from ModelPicker. Triggered by `tool.approval_requested`; composer currently goes inert during busy mode (`ComposerRegion.tsx:47`, `App.tsx:184`) so showing approval input requires real App-state change. **Must land before APR-03 enforces approvals**, or runs will hang or require a hidden feature gate. |
| INK-04 | `tool.approve` / `tool.reject` client methods | weak | Add to `NavBackendClient`; current client only exposes `session.create` / `session.sendMessage` (`client.ts:7`). Depends on APR-02a being merged. |
| INK-05 | Streaming output rendering in tool cells | strong | Teach `ToolCallCell` to append `tool.output_delta` chunks live; bounded scroll inside the cell (cap visible lines, "…N more lines" tail) so the history region doesn't reflow on every chunk. App-state change: streaming cells must coexist with the busy/approval state. Depends on INK-02 and APR-04. |

## Phase 4 — Search + mutation

Goal: nav can search the codebase fast and change files safely.

| # | Issue | Model | Scope |
|---|---|---|---|
| TOOL-07 | `ripgrep` tool | weak | LLM-facing tool name is `ripgrep`; spawns system `rg` via **injectable command resolver** (so tests aren't machine-sensitive). Timeout + output cap reuse the `bash`-style utilities. Hard-error if `rg` missing with install hint. No GNU-grep fallback. Lands before edit/write so the agent can find what to change. |
| TOOL-08 | File mutation queue + `write` tool | strong | Bundled: `tokio::sync::Mutex` keyed on canonical path is abstract on its own. `write` is the simpler mutation tool and proves the queue. Mutation-safety work — relabeled strong from rev-1's weak. Uses TOOL-03 path policy; approval-gated for out-of-workspace paths via APR-01. |
| TOOL-09 | `edit` tool with exact-match semantics | strong | **Acceptance spells out behavior for zero / multi / unique match per `oldText`**, including structured error returned to the model. Port Pi's unique-match rules; no fuzzy patch. Uses TOOL-08 queue. |
| TOOL-10 | `file.changed` event end-to-end | strong | Harness emit + protocol mapping + INK payload variant (TUI may render a quiet chip or no-op for v1 but the type must exist). Cross-layer — depends on INK-01. Relabeled strong from rev-1's weak. |

## Sequencing

```
LOOP-01 ─► LOOP-02 ─► LOOP-03 ─► LOOP-04
                        │
                        ▼
TOOL-01 ─┬─► TOOL-02
         └─► TOOL-03 ─► TOOL-04 ─┬─► TOOL-05
                                  └─► TOOL-06
                                        │
INK-01 ─► INK-02 ◄───────────────────────┘

APR-01 ─► APR-02a ─► APR-02b ─► APR-03 ─► APR-04
              │                    ▲          │
              ▼                    │          ▼
            INK-04                 │        INK-05
                                   │          ▲
INK-03 ───────────────────────────┘           │
       (INK-03 must merge before APR-03)      │
INK-02 ───────────────────────────────────────┘

TOOL-07
TOOL-08 ─► TOOL-09
INK-01 ─► TOOL-10
```

Critical path = the strong-model chain LOOP-01 → LOOP-03 → TOOL-01 → TOOL-03 → TOOL-04 → APR-01 → APR-02a/b → APR-03, with INK-01 → INK-02 → INK-03 running in parallel and converging before APR-03 enforces.

## Open decisions

1. **Prefix style** — four groups (LOOP / TOOL / APR / INK) vs single umbrella (AG-\*).
2. **`INK-*` vs `TUI-*`** — prefer INK-\* to avoid confusion with closed Go-TUI issues; confirm.
3. **TOOL-04 bundling** — encoding + `read` + dispatch in one PR (Codex recommendation, current plan) vs three sequential strong PRs (more granular but each is abstract). Tradeoff: one bigger PR is more reviewable as a coherent slice; three small PRs are easier to revert.
4. **TOOL-05 fold-in** — keep `ls` as a weak issue or fold into TOOL-04.
5. **Phase 4 ordering** — `ripgrep` before mutation tools (current plan, lets agent find what to change) vs after (smaller risk surface first).
6. **Bash output streaming chunk size / flush cadence** — APR-04 / INK-05 are in scope; remaining decision is how often the harness coalesces stdout chunks before emitting `tool.output_delta` (per-line, time-batched, byte-threshold). Default suggestion: 50 ms batched windows, max 4 KB per event.

## Out of scope (deferred)

- Session storage to disk (the `session-storage.md` plan)
- MCP integrations (`integrations/mcp.rs`)
- Skills (`skills/`)
- Image reads in the `read` tool
- Anthropic tool-call encoding
- `find` tool (ls + ripgrep cover it; Huntley skips it too)
- Managed-binary downloads for `rg` / `fd` (require system PATH for v1)
- User-facing bash from the composer (`!cmd` style)
