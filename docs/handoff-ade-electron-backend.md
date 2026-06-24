# Handoff — ADE (Electron) backend selection & Pi tool reuse

**Date:** 2026-06-24
**Status:** Decision reached; ready to start a code spike.

## Goal

The user is building an **ADE (Agentic Development Environment)** — a Claude-Code-style
desktop app — with **Electron** as the desktop shell. They asked which of four backends
is most suitable as the agent backend.

Candidates evaluated:
1. `withastro/flue` — local clone: `/Users/season/Personal/flue`
2. `earendil-works/pi` — local clone: `/Users/season/Personal/pi`
3. `TanStack/ai` — local clone: `/Users/season/Personal/ai` (NOT vercel; confirmed via its CLAUDE.md)
4. `vercel/ai` — not cloned locally; assessed from general knowledge

## Decisions reached (in order)

### 1. Backend category: use an agent *harness*, not an AI *SDK*
- **Harnesses** (autonomous agent runtime — what an ADE backend actually is): **Flue**, **Pi**.
- **SDKs** (LLM-call + streaming + tools + UI hooks — building blocks): **TanStack AI**, **Vercel AI**.
- An ADE's defining trait is an autonomous, tool-using, sandboxed agent. Harnesses ship that;
  SDKs make you build it.
- **Recommendation: Flue** (primary) — ships sandboxes, sessions, durable execution, subagents,
  skills, MCP, observability, Postgres persistence; TS/Node so embeds in Electron main process
  or runs as a sidecar.
- **Pi** (runner-up) — pick if the ADE is narrowly a *coding* agent and you want a working
  agent today; trade-off is you supply your own sandbox/permission layer and it's TUI-first.
- **TanStack AI / Vercel AI** — wrong for the backend, but right for the **Electron renderer**
  UI (e.g. Vercel's `useChat`) streaming the harness output. Common arch: Flue backend +
  Vercel AI SDK hooks in renderer.

### 2. Flue is built on *part* of Pi
- `@flue/runtime` depends on `@earendil-works/pi-agent-core ^0.79.4` and `@earendil-works/pi-ai ^0.79.4`
  (see `/Users/season/Personal/flue/packages/runtime/package.json` lines 67–68).
- It does **not** depend on `pi-coding-agent` or `pi-tui`.
- Pi's layering: `pi-ai` (provider abstraction) + `pi-agent-core` (engine: agent loop, tool calling,
  state) are the *engine*; `pi-coding-agent` (CLI app) + `pi-tui` (terminal UI) are *applications*.
- So **Flue and `pi-coding-agent` are siblings on the same engine**, not parent/child.
  "No coding agent in Flue" is by design — Flue is a framework; you author the agent via
  `createAgent(() => ({ model, tools, skills, sandbox, instructions }))`.

### 3. "Flue + pi-coding-agent" doesn't nest — harvest instead
- Both are top-level harnesses over `pi-agent-core`; both want to own the run loop. Stacking them
  = harness-on-harness (one goes unused).
- `pi-coding-agent` is an app/CLI (`"bin": { "pi": "dist/cli.js" }`) that drags in `pi-tui`.
- Two real options to "have both": **(a) harvest** its tools + prompts into a Flue agent (recommended);
  **(b) nest** the `pi` CLI as a subprocess tool inside a Flue sandbox (works literally, but gives
  two agent loops / two session systems — usually wrong for Electron).

### 4. Toolset reuse analysis (the active thread)
Tool files live at: `/Users/season/Personal/pi/packages/coding-agent/src/core/tools/`
(`read.ts`, `bash.ts`, `edit.ts`, `write.ts`, `grep.ts`, `find.ts`, `ls.ts` + helpers
`path-utils.ts`, `truncate.ts`, `edit-diff.ts`, `file-mutation-queue.ts`, `output-accumulator.ts`,
`render-utils.ts`, `tool-definition-wrapper.ts`).

Findings:
- **All 7 tools are publicly exported** from `pi-coding-agent`'s `main` (`src/index.ts`):
  `createReadTool` … `createLsTool`, plus `createCodingTools(cwd)` / `createReadOnlyTools(cwd)` /
  `createAllTools(cwd)` and the `*ToolDefinition` variants.
- Factories return **`AgentTool` from `@earendil-works/pi-agent-core`** — the exact engine type
  Flue drives. So they slot into Flue's `createAgent({ tools })` with zero adaptation (same 0.79.x line).
- **Catch:** every tool file statically imports `pi-tui` + `modes/interactive` (3–4 imports each)
  for terminal *rendering*. These can't be tree-shaken (the factory's `ToolDefinition` references the
  render fns). So `import { createReadTool }` drags pi-tui, themes, highlight.js, photon-node wasm
  into the Electron bundle — dead weight, since `wrapToolDefinition` only copies
  `name/description/parameters/execute` to the `AgentTool` (renderers never run under Flue).
- **Integration seam (matters most):** each tool takes a pluggable `operations` option
  (`ReadOperations`, `BashOperations` w/ `BashSpawnHook`) explicitly built to delegate to remote
  systems. **Inject custom `operations` so file/shell ops route through Flue's sandbox** instead of
  host fs. Identical work whether you import or copy.

Import-vs-copy verdict:
- **Prototype → import directly** (`createCodingTools(cwd)`). Proves engine compatibility in minutes.
- **Production Electron ADE → copy** the 7 tool modules + local helpers, **delete the ~3 TUI/
  `modes/interactive` import lines + render functions per file**, keep `execute` + schemas +
  `operations`. MIT-licensed, clean. Render in the Electron renderer instead. Port is mechanical —
  TUI symbols are display-only, not referenced inside `execute` (verified for `read.ts`).

## Immediate next step (offered, not yet done)
Draft a **stripped-down `read.ts`** (TUI imports + render fns removed) wired to a **Flue sandbox
`operations`**, as the copy/strip template for the other 6 tools. User's last shell `pwd` confirms
they're sitting in the tools dir ready for this.

## Open questions for the user
- Confirm "ADE" = Agentic Development Environment (assumed throughout).
- Is the ADE narrowly a coding agent (favors harvesting Pi tools) or broader?
- Prototype-first (import) or go straight to the production copy/strip path?
- Renderer UI choice: Vercel AI `useChat` vs TanStack AI hooks.

## Key reference paths
- Flue runtime deps: `/Users/season/Personal/flue/packages/runtime/package.json:67`
- Flue README (feature list): `/Users/season/Personal/flue/README.md`
- Pi README (package layering): `/Users/season/Personal/pi/README.md`
- pi-coding-agent public API: `/Users/season/Personal/pi/packages/coding-agent/src/index.ts`
- pi-coding-agent package shape: `/Users/season/Personal/pi/packages/coding-agent/package.json`
- Tools + wrapper: `/Users/season/Personal/pi/packages/coding-agent/src/core/tools/`
- Pi sandboxing/containerization patterns: `/Users/season/Personal/pi/packages/coding-agent/docs/containerization.md`

## Suggested skills for the next agent
- **`flue`** — version-matched Flue docs (agents, sandboxes, tools, sessions). Invoke before
  writing any Flue `createAgent` / sandbox `operations` code.
- **`deepwiki`** (MCP) — confirm current `pi-agent-core` `AgentTool` contract and Flue sandbox API
  before relying on memory; both move fast.
- **`run`** — once a spike exists, launch the Electron app to verify the tool wiring end-to-end.
- **`code-review`** — review the copied/stripped tool files before they land.
