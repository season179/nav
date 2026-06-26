# Plan — Rebuild nav on Flue (backend) + TanStack (renderer)

**Format:** goals and **acceptance criteria**, not step-by-step instructions. You (the implementing
agent) decide *how*. This document tells you *what must be true when you're done*, the *hard
constraints* you must not violate, and the *context/findings* so you don't rediscover them or walk
into known dead-ends.

**Branch:** `revamp/flue` (checked out). **Last updated:** 2026-06-24.

> **Run fully autonomously. Never stop to ask the user a question.** When you hit uncertainty,
> ambiguity, or a missing detail, make the best decision you can from this document, the codebase, and
> the Flue/TanStack docs — then proceed. Prefer the choice that keeps §0's acceptance criteria
> satisfiable and is easiest to reverse. Record non-obvious decisions (and any `TODO(verify)`) in the
> commit message or backend README so they're auditable later. A reasonable decision made now beats a
> blocked run.

---

## 0. Mission & definition of done

Replace nav's deleted Rust backend with a **Flue** backend (a coding agent), and rebuild the
**Electron renderer** to **maximize TanStack** usage. nav is a single-user desktop coding-agent app.

**The rebuild is DONE when all of the following hold — and every check is runnable offline (no model
API key):**

1. The Electron app launches, spawns the Flue backend, and the renderer reaches it. A user can create
   a session, type a message, and the request is accepted and streamed back as a live transcript
   (verified with a stubbed/mock model or recorded events — see §1, §9).
2. Everything the old frontend needed still works end-to-end: session create/list/latest/resume,
   send, stop, model info/list/switch, thinking switch, stacks + availability, and `local`/`worktree`
   modes. (You may change the *wire protocol* used to deliver these — see §3 — as long as the
   user-facing behavior holds.)
3. The renderer genuinely uses the TanStack libraries that fit (§8), not as decoration.
4. All quality gates in §7 pass green. Live-model behavior is covered only by the manual checklist
   (§10) and does **not** gate any milestone.
5. The repo is clean: no dead Rust/cargo references, backend + renderer + main all build, and the
   tree is green at every commit.

If you cannot satisfy a criterion, **leave a clearly-marked `TODO(verify)` with the exact doc query
to run** and keep going — never fabricate an API to make a check pass.

---

## 1. Hard constraints (non-negotiable)

- **Tooling:** pnpm 11, Node 24 (pinned via `.nvmrc` + `engines`). Never `bun`, never `npm` in this
  repo. Use `pnpm exec` / `pnpm dlx` for the Flue CLI.
- **No model key in any gate.** A real provider key is unavailable to CI and the unattended run.
  Every milestone's acceptance check must pass offline. Anything needing a real model turn is
  manual-only (§10) and must not block a milestone or a commit.
- **Verify fast-moving APIs before coding against them.** This plan was written against the Flue docs
  bundled on 2026-06-24; packages move. Use the `flue` skill (`flue docs read <path>` /
  `flue docs search <query>`) and `deepwiki` to confirm Flue/`@flue/sdk` shapes; confirm current
  TanStack package names + peer ranges and Node 24 `node:sqlite` stability before adding deps. **The
  installed package wins over this document.**
- **Commits:** human voice, imperative subject, one focused commit per milestone (or sub-step).
  **No** "Co-Authored-By", **no** "Generated with Claude Code", no AI attribution. Keep the tree green
  at every commit.
- **Scope discipline:** ship a working Flue + TanStack nav, not a rewrite of everything. Reuse code
  that already works (CSS, markdown rendering, window-security, OS IPC, the pure transcript reducers).
- **Autonomy:** never pause for user input. Resolve every uncertainty yourself with the best available
  decision (see the callout at the top), document it, and continue.

---

## 2. Current state (context)

- **Electron app present and working** under `desktop/electron/` (main `main.cts`, sandboxed
  `preload.cts`, transport `backend-client.cts`/`backend-process.cts`, React renderer under
  `renderer/`). Plain React 19 + Vite; no TanStack yet.
- **Rust backend removed** (commit `fcbe5945`). Two integration tests in
  `tests/electron_backend_client.test.cts` fail because they spawn the deleted `cargo` binary —
  **expected**; they should go green (repointed) or be removed once Flue serves the contract.
- **Tooling already migrated** (commit `2e8391f7`): pnpm 11 + Node 24 pinned; `pnpm-workspace.yaml`
  has `allowBuilds` + `engineStrict`; `.nvmrc` = `24.16.0`.

---

## 3. The contract & domain facts (reference — don't rediscover)

The Electron **main process** brokers renderer↔backend today via JSON-RPC (`POST /rpc`) + SSE
(`GET /sessions/:id/events`), and detects backend readiness from a **stdout** line:

```
nav local backend listening on <url>
```

`backend-process.cts` polls stdout for that exact prefix for ~60s. **You may keep or replace the
JSON-RPC/SSE wire format** — the renderer is being rewritten anyway, so a clean redesign is fine —
but **keep emitting the startup line** (cheap, and the spawn watcher already depends on it) unless you
also update the watcher in the same change.

### 3.1 The behaviors that must survive (the old 12 RPC methods)

| Capability | Params (as used) | Returns (as consumed) |
| --- | --- | --- |
| create session | `{ cwd, mode }` | `{ sessionId }` |
| list sessions | — | `{ sessions: SessionSummary[] }` |
| latest session | `{ cwd }` | `{ sessionId } \| null` |
| resume session | `{ sessionId }` | `{ sessionId }` |
| send message | `{ sessionId, text }` | accepted (then events stream) |
| stop run | `{ sessionId }` | `{ stopped: boolean }` |
| model info | `{ sessionId? }` | `ModelInfo` |
| model list | — | `{ models: ModelOption[] }` |
| switch model | `{ sessionId, provider, model, thinkingLevel? }` | `{ modelInfo }` |
| switch thinking | `{ sessionId, thinkingLevel }` | `{ modelInfo }` |
| stacks | `{ sessionId }` | `SessionStacksResult` |
| stack availability | `{ sessionId }` | `{ available }` |

`mode` ∈ `"local" | "worktree"` (worktree = a git worktree per session).

### 3.2 Existing renderer types (in `renderer/src/types.ts`) — reuse these shapes

- `SessionSummary` (`:30`): `{ sessionId, title, workspaceRoot, projectRoot, updatedAt }`.
- `ModelOption` (`:38`): `{ provider, model, label, thinkingLevels[] }`.
- `ModelInfo` (`:50`): `{ label, provider?, model?, thinking?, thinkingLevels[], tokenUsage? }`;
  `TokenUsage` (`:45`): `{ used, contextWindow }`.
- `Message` (`:95`) = `ChatMessage { id, role: user|assistant|error, text, createdAt }`
  ∪ `ToolMessage { id, role: tool, toolCallId, state, toolName, detail }`.
- `SessionState` (`:114`): `{ messages, running, stopPending, modelInfo, stackAvailable,
  stackRefreshKey, messageSeq }`.
- Stacks: `StackEntry { id, runId, sequence, status, startedAtMs, durationMs, request?, response? }`,
  `StackRequest { api, url, model, body? }`, `StackResponse { statusCode?, body?, error?,
  tokenUsage? }`, `SessionStacksResult { stacks[], unavailableReason? }`.

### 3.3 Reuse the pure transcript reducers
`lib/session-runtime.ts` already has **pure** `reduceSessionState(state, event)` (`:114`) and
`reduceSessions(states, event)` (`:173`) plus dedup-by-`event_id` and tool-line upsert-by-`toolCallId`
logic, with existing tests. **Reuse this model and its tests.** Adapt the *input* (the event the
reducer switches on) rather than rewriting the reducer — a thin "Flue event → nav event" adapter in
front keeps the proven logic intact.

---

## 4. Flue facts you'll rely on (reference)

Confirmed from the bundled Flue docs (verify against the installed package before coding):

- **An addressable agent instance = a nav session.** Put the agent at `src/agents/nav.ts` (discovered
  as agent name `nav`). It must `export const route` for HTTP routes to mount. Native routes:
  `POST /agents/nav/:id` (send `{ message, images? }`, returns `202 { streamUrl, offset }`) and
  `GET /agents/nav/:id?live=sse&offset=…` (Durable Streams SSE). (Refs: `routing-api`, `sdk/agents`,
  `streaming-protocol`.)
- **The coding toolset is built into the sandbox.** `sandbox: local()` (from `@flue/runtime/node`)
  gives the agent read/write/edit/grep/glob/bash in its `cwd`. **You do not need to hand-wire file
  tools.** (Ref: `ecosystem/deploy/node`.) Harvesting pi's tools is optional polish only.
- **The agent initializer re-runs per interaction** and receives `ctx.id` (the instance id). So
  per-session config (model, thinking, cwd) can be looked up by id at init time. (Ref: `agent-api`.)
- **Flue has no "list agent instances" API.** `listAgents()` lists agent *modules*. nav must keep its
  own session catalog for list/latest/resume. (Ref: `data-persistence-api`.)
- **Persistence** is via a `src/db.ts` exporting `sqlite('./data/flue.db')` (built-in). It stores
  conversation history, not your catalog. (Ref: `guide/database`.)
- **Models** are just provider/model specifier strings; no "list models" API exists — nav owns the
  curated list. Reasoning effort is `thinkingLevel` ∈ off|minimal|low|medium|high|xhigh. Keys come
  from env (`ANTHROPIC_API_KEY`, etc.), read by the provider layer, **not** the sandbox. (Ref:
  `guide/models`.)
- **`turn_request` (the raw model-visible request) is in-process only — never persisted, never served
  over HTTP.** It's only reachable via `observe()` inside the server. This matters for "stacks" (§5,
  §9). Other events (`tool_start`/`tool`, `message_start`/`message_end`, `operation`/`idle`,
  `text_delta`, `turn`) are on the stream. Detailed message payloads are pre-1.0 unstable — branch
  defensively on the `v:1` envelope. (Ref: `events-reference`.)
- **No documented mid-turn HTTP cancel.** `send` returns `202` and the durable submission keeps
  running; aborting the HTTP request doesn't stop the loop. This makes a guaranteed server-side
  `stop` an open question (§9).
- `app.ts` (Hono) is the place to mount `flue()` alongside your own routes (`/health`, a nav
  control-plane, etc.). (Ref: `routing-api`.)

---

## 5. Recommended architecture (strong default — deviate only if you can still meet §0 and justify it)

```
Electron MAIN (supervisor + OS bridge): spawn Flue server, detect startup line, hand baseUrl to
  renderer, own the directory picker + "Start in" mode pref. Stays small.
        │  preload exposes baseUrl + OS-only IPC
        ▼
RENDERER (React 19 + TanStack): Query = data layer over HTTP to the local Flue server; Router = views;
  Store = live transcript fed by an SSE reader; Virtual/Form/Table/Pacer/Devtools per §8.
        │  HTTP to 127.0.0.1:PORT (single origin)
        ▼
FLUE BACKEND (backend/, Node target): src/agents/nav.ts (model + instructions + local() sandbox);
  src/app.ts (flue() + nav control-plane + /health + startup line); src/db.ts (sqlite); a nav session
  catalog (node:sqlite or JSON) for list/latest/resume; a static model catalog; worktree creation; a
  stacks store fed by observe().
```

Why this shape (so you can judge deviations):

- **A nav session = a Flue instance id** → multi-turn conversation maps cleanly to the addressable
  agent; run + stream use native Flue routes.
- **Data plane = direct renderer→Flue HTTP** → TanStack Query/SSE are first-class; OS-only work
  (picker, pref file) stays on IPC because the renderer can't do it.
- **A nav-owned catalog** is required because Flue can't enumerate instances (§4). `session.list`
  must return `SessionSummary[]` (§3.2). Keep it in a store separate from Flue's `db.ts`.
- **Per-session model/thinking** via the initializer reading the catalog by `ctx.id` (the native send
  route has no model field).
- **Stacks** = captured model turns. Because `turn_request` is in-process-only (§4), faithful stacks
  require an `observe()` subscriber in the backend that stores sanitized turn request/response rows
  keyed by session; the stacks route reads that store. Shipping a clean "unavailable" stub first is
  acceptable.
- **Do NOT** reimplement JSON-RPC, write a custom persistence adapter, use a remote sandbox, or pull
  in `@flue/react` (you're maximizing TanStack; `@flue/sdk` is fine purely as HTTP/stream transport).

You may choose differently (e.g., keep the JSON-RPC contract, or use `@flue/react`) **only** if §0
still holds and you record the rationale in the backend README.

---

## 6. Milestones — each defined by acceptance criteria (you choose the implementation)

Work roughly in this order (later milestones depend on earlier ones). For each: the criteria are what
must be true; the **Verify** line is the offline check.

### M0 — Workspace ready
**Done when:**
- The repo is a 2-package pnpm workspace (Electron app + a `backend/` Flue project); the renderer
  keeps building as today.
- Lint/format config covers the backend; generated build dirs and secrets are gitignored.
**Verify:** `pnpm install` clean; `pnpm run typecheck` + `pnpm run lint` green.

### M1 — Backend boots and defines the coding agent
**Done when:**
- A Flue Node server builds and starts with **no** model key, prints the startup line on stdout, and
  answers `GET /health` and `GET /openapi.json`.
- A `nav` agent is defined with a `local()` sandbox and a `cwd` (so it has built-in coding tools), and
  exports `route`.
- Session conversation state persists across restart (sqlite `db.ts`).
**Verify:** backend typecheck + build; start it keyless → startup line appears, `/health` +
`/openapi.json` respond. (A real prompt failing without a key is expected and out of scope.)

### M2 — Control-plane satisfies the session/model/stacks/worktree surface
**Done when (all offline-testable):**
- Every capability in §3.1 is reachable over HTTP (native Flue route for send/stream; nav
  control-plane for the rest). `session.list` returns `SessionSummary[]`; `modelInfo` returns
  `ModelInfo` (incl. `thinkingLevels` and a `tokenUsage` shape); the model list returns
  `ModelOption[]`.
- `local` mode works fully; `worktree` mode creates/tracks a git worktree and points the agent's `cwd`
  at it. The catalog persists sessions (separate from Flue's db).
- Stacks + availability return correctly-shaped data. Faithful capture via `observe()` is implemented
  **or** a clean "unavailable" stub is returned with the capture store + reducer written and tested.
- `stop` returns an honest `{ stopped }`; the §9 cancellation question is resolved or flagged.
**Verify:** backend typecheck + unit tests (catalog CRUD + latest-by-cwd; model/`modelInfo` shapes;
worktree path with mocked git; stacks capture reducer over synthetic `turn_request`/`turn`; route
handlers via Hono `app.request`). Manual `curl` of create→list→latest→resume→delete works.

### M3 — Electron launches the Flue backend; renderer can reach it
**Done when:**
- Main spawns the Flue server (not cargo), detects the startup line, and the resolved backend URL is
  available to the renderer.
- The OS-only surface (directory picker, "Start in" mode pref) still works; obsolete RPC passthroughs
  are gone or repointed.
- The 2 formerly-failing backend tests are removed or repointed; remaining main/preload tests stay
  green.
**Verify:** `pnpm run check:electron` green (typecheck + main build + renderer build + `node --test
tests/*.test.cts`). App boots, window loads, renderer obtains the backend URL.

### M4 — Live events render as a transcript (pure, fixture-tested)
**Done when:**
- The renderer reads a Flue agent SSE stream (data/control/heartbeat frames; resumes via the control
  frame's next offset) and reduces events into the existing `SessionState`/`Message` model.
- The Flue-event→transcript mapping is a **pure function**, reusing `lib/session-runtime.ts`, covered
  by tests over synthetic Flue event arrays (run start, text, tool start/end ok+error, message end,
  completion, error).
**Verify:** parser + adapter/reducer unit tests green. No live stream needed.

### M5 — Renderer is TanStack-maximized
**Done when:**
- The data layer is TanStack Query (the §3.1 reads/writes; cache keyed by session id; invalidation
  driven by stream events instead of the old manual refresh web).
- The libraries in §8 are each used where they genuinely fit (that table is the acceptance list).
- All views render correctly with **mocked** data: chat (composer + transcript), sidebar (sessions),
  models picker, settings (model/thinking + mode), stacks.
- Dead plain-React data plumbing superseded by TanStack is removed.
**Verify:** `pnpm run renderer:build` + `pnpm run typecheck` + `pnpm run lint` green; hook tests
(mocked fetch) + reducer tests green; each view renders under a mock-data harness.

### M6 — (Optional) harvest pi tools for richer tool UX
Only if the built-in sandbox tools are insufficient. **Done when:** the copied/stripped pi tools (TUI
imports removed, `execute` + schemas + `operations` kept) register via `createAgent({ tools })`, the
agent still boots, and tool definitions validate. See `docs/handoff-ade-electron-backend.md` §4.
**Verify:** backend typecheck + boot.

### M7 — Polished & documented
**Done when:** no dead Rust/cargo references or stale comments; `backend/README.md` documents
build/run, the catalog/db split, the resolved `stop` and `stacks` behavior, the model catalog,
worktree behavior, and where the real provider key goes; root docs describe the Flue + TanStack
architecture.
**Verify:** full §7 gates green end-to-end.

---

## 7. Quality gates (the offline commands that must pass)

- Backend: `cd backend && pnpm run typecheck && pnpm exec vitest run` (or your chosen runner) +
  `pnpm run build` produces a runnable server that boots keyless.
- Electron/renderer: `pnpm run check:electron` (typecheck + main build + renderer build +
  `node --test tests/*.test.cts`).
- Whole repo: `pnpm run lint` clean.
- None of these may require a model API key.

---

## 8. TanStack coverage (acceptance: each library is genuinely used where it fits)

"Maximize" means use every library that *honestly fits*, not bolt them on. Acceptance per library:

| Library | Must be used for | 
| --- | --- |
| **Query** | All §3.1 reads/writes; cache per session id; event-driven invalidation. |
| **Router** | App views (chat / per-session / stacks / settings); deep-link the active session id. (Confirm history works under Electron `file://`; use memory/hash history if needed.) |
| **Store** | Live transcript + run state fed by the SSE reducer. |
| **Virtual** | Transcript message list (unbounded growth) and the sidebar session list when long. |
| **Form** | Settings (model/thinking), "Start in" mode, composer validation. |
| **Table** | Models picker and stacks tables (sort/filter). |
| **Pacer** | Debounce composer draft / throttle typing-driven work / rate-limit stream reconnect. |
| **Devtools** | Query + Router (+ Store if supported), dev builds only. |
| **DB** | *Optional.* Adopt only if reactive collections clearly beat Store for cross-view reads; otherwise skip. |
| **Ranger / Start / Config** | Not used (no range UI; Start is SSR; Config not needed). State this in the PR. |

If a library has no honest use, say so in the PR rather than forcing it.

---

## 9. Known unknowns — acceptance = resolved and documented honestly

1. **`session.stop` (mid-turn cancel).** No documented HTTP cancel (§4). Acceptance: implement an
   honest server response **and** a real client-side stop (drop the stream, freeze the optimistic UI,
   mark the run cancelled); investigate whether the installed Flue/SDK exposes submission
   cancellation or an `AbortSignal` that propagates (`flue docs search "cancel"`/`"abort"`/`"signal"`)
   and document the actual behavior. Do not claim a guaranteed kill you didn't verify.
2. **Stacks capture.** `turn_request` is in-process-only (§4). Acceptance: either implement the
   `observe()` capture→sanitize→store path, or ship a clean "unavailable" stub with the store/reducer
   written + tested. Document which.
3. **Startup-line / `listen()` seam.** Confirm how the built Flue Node server exposes its bound
   address so the startup line reflects the real (possibly ephemeral) port; if the generated entry
   owns `listen()`, print from a ready path or pin `PORT` from main. Document the approach.
4. **First-run provider setup.** If no usable model/provider configuration exists, the Electron app
   should show a setup path instead of letting the user submit into a backend error. Initial scope:
   detect the empty config state, explain what is missing, and link or write to the nav settings file;
   full credential/provider management can come later.

For each: if unresolved at the end, leave a `TODO(verify)` naming the exact doc query to run.

---

## 10. Manual smoke checklist (human, with a real key — never a gate)

1. Put a provider key in `backend/.env`. Build + start the backend; `curl -XPOST
   127.0.0.1:PORT/agents/nav/test -d '{"message":"list files"}'` streams tool + text events.
2. Launch the Electron app: create a session, send a message, watch it stream, switch model, open
   stacks, stop a run, relaunch and confirm resume.
3. Capture a real event array and fold it into the M4 reducer fixtures to harden tests.

---

## 11. Reference index

- Flue agent/sessions/tools: `flue docs read api/agent-api`, `guide/tools`, `guide/building-agents`.
- Flue HTTP/streaming: `flue docs read api/routing-api`, `api/streaming-protocol`, `sdk/agents`.
- Flue events: `flue docs read api/events-reference`. Persistence/sandbox/models/deploy:
  `guide/database`, `guide/sandboxes`, `guide/models`, `ecosystem/deploy/node`,
  `api/data-persistence-api`.
- pi tool harvest: `docs/handoff-ade-electron-backend.md`.
- Current Electron contract & types: `desktop/electron/{main,preload,backend-process,backend-client}.cts`,
  `desktop/electron/renderer/src/types.ts`, `desktop/electron/renderer/src/lib/session-runtime.ts`.
