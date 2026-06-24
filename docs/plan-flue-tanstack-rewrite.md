# Plan — Rebuild nav on Flue (backend) + TanStack (renderer)

**Status:** Ready to execute. Written for an autonomous agent to work through over a long
(multi-hour) session.
**Branch:** `revamp/flue` (already checked out).
**Last updated:** 2026-06-24.

> This document is the single source of truth for the rewrite. It is intentionally large and
> prescriptive. Work the phases **in order**. Each phase ends with a **verification gate** you can
> run *without a model API key*. Do not skip a gate. When a step says "verify against current Flue,"
> use the `flue` skill (`flue docs read <path>` / `flue docs search <query>`) and the `deepwiki` MCP
> before relying on any API shape — Flue and the Pi packages move fast and this plan was written
> against the docs bundled on 2026-06-24.

---

## 0. How to use this document

- **Audience:** an autonomous coding agent with shell + file access, running unattended.
- **Golden rule:** never invent a Flue/SDK API. If a symbol or route in this plan does not match the
  installed package, stop and check the docs (`flue docs`), then adapt. Prefer the package over this
  plan when they disagree.
- **No live-model testing.** A real `ANTHROPIC_API_KEY` (or equivalent) is *not* available to CI or
  to the overnight run. Every verification gate must pass offline. Anything that needs a real model
  turn is explicitly deferred to a human "manual smoke" checklist (§11) and must **not** block a phase.
- **Commit per phase.** End each phase with a single focused commit (see §12 for message style — no AI
  attribution, human voice). Keep the tree green (`pnpm run check:electron` + backend gates) at every
  commit.
- **Tooling:** pnpm 11, Node 24 (pinned via `.nvmrc` + `engines`). Never use `bun` or `npm` in this
  repo. With Flue's CLI, prefer `pnpm exec flue …` / `pnpm dlx` over global installs.

---

## 1. Current state (snapshot)

What exists on `revamp/flue` right now:

- **Electron app (frontend), intact.** `desktop/electron/` — main process (`main.cts`), sandboxed
  preload (`preload.cts`), backend transport (`backend-client.cts`, `backend-process.cts`), and the
  React renderer (`desktop/electron/renderer/`). Plain React 19 + Vite; no TanStack yet.
- **Rust backend, removed.** Commit `fcbe5945` deleted the Rust `nav-local-backend`. The two backend
  integration tests (`tests/electron_backend_client.test.cts`) now fail because they spawn the deleted
  `cargo` binary — **expected**, they go green again once Flue serves the contract.
- **Tooling migrated.** Commit `2e8391f7` switched to pnpm 11 + Node 24. `package.json` pins
  `packageManager: pnpm@11.9.0`, `engines.node: ">=24.16.0 <25"`. `pnpm-workspace.yaml` holds
  `allowBuilds` + `engineStrict`. `.nvmrc` = `24.16.0`.

### 1.1 The contract the frontend still speaks (must be satisfied)

The Electron **main process** brokers between renderer and backend. Two transports:

1. **JSON-RPC 2.0** over `POST /rpc` — `sendRpc({ backendUrl, method, params })` in
   `backend-client.cts`. 12 methods (see §1.2).
2. **SSE** over `GET /sessions/:id/events` — `subscribeToSessionEvents(...)` in `backend-client.cts`.
   11 event types (see §1.3).

The backend announces readiness by printing to **stdout**:

```
nav local backend listening on <url>
```

`backend-process.cts` polls stdout for this exact prefix (`STARTUP_PREFIX`,
`backend-process.cts:5`) for up to 60s (1200 × 50ms). It also reads structured startup-trace lines
from **stderr** prefixed `nav startup trace ` (`STARTUP_TRACE_PREFIX`, line 6) — optional, used by
`startup-trace.cts`.

> **Decision (see §3):** we will *redesign* this main↔backend contract rather than reimplement the
> bespoke JSON-RPC in Flue. The renderer is being rewritten for TanStack anyway, so preserving the
> old wire format buys nothing. We keep the **startup line** (cheap, and `backend-process.cts` already
> depends on it) and replace `/rpc` + `/sessions/:id/events` with **Flue's native agent HTTP API**
> plus a small **nav control-plane** router.

### 1.2 The 12 RPC methods (current params → returns, from `main.cts`)

| Method | Params | Returns (as consumed) | New home (see §3, §9) |
| --- | --- | --- | --- |
| `session.create` | `{ cwd, mode }` | `{ sessionId }` | nav control: `POST /nav/sessions` |
| `session.list` | — | `{ sessions: [...] }` | nav control: `GET /nav/sessions` |
| `session.latest` | `{ cwd }` | `{ sessionId } \| null` | nav control: `GET /nav/sessions?latest&cwd=` |
| `session.resume` | `{ sessionId }` | `{ sessionId }` | nav control: `GET /nav/sessions/:id` (catalog lookup) |
| `session.sendMessage` | `{ sessionId, text }` | accepted | Flue native: `POST /agents/nav/:id` |
| `session.stop` | `{ sessionId }` | `{ stopped: boolean }` | nav control: `POST /nav/sessions/:id/stop` (best-effort, §8) |
| `session.modelInfo` | `{ sessionId? }` | model info object | nav control: `GET /nav/sessions/:id/model` |
| `session.models` | — | `{ models: [...] }` | nav control: `GET /nav/models` |
| `session.switchModel` | `{ sessionId, provider, model, thinkingLevel? }` | `{ modelInfo }` | nav control: `PUT /nav/sessions/:id/model` |
| `session.switchThinking` | `{ sessionId, thinkingLevel }` | `{ modelInfo }` | nav control: `PUT /nav/sessions/:id/thinking` |
| `session.stacks` | `{ sessionId }` | stacks result | nav control: `GET /nav/sessions/:id/stacks` |
| `session.stackAvailability` | `{ sessionId }` | availability result | nav control: `GET /nav/sessions/:id/stacks/availability` |

`mode` ∈ `"local" | "worktree"`. Worktree mode creates a git worktree per session (artifacts seen
under `.nav/worktrees/`). See §3.6.

### 1.3 The 11 SSE event types (current)

`connected`, `user.message`, `run.started`, `assistant.tool_calls`, `tool.started`,
`tool.completed`, `tool.failed`, `message.completed`, `run.completed`, `run.cancelled`,
`run.failed`. Mapping from Flue's native event union → the renderer's transcript model is in §7.

---

## 2. Target architecture

```
┌──────────────────────────────────────────────────────────────────────────┐
│ Electron MAIN process  (process supervisor + OS bridge — stays small)      │
│  • spawns the Flue Node server:  node backend/dist/server.mjs              │
│  • waits for stdout line:  "nav local backend listening on http://127…:PORT"│
│  • exposes resolved baseUrl to the renderer (preload: window.nav.*)        │
│  • OS-only IPC: directory picker (dialog), "Start in" mode pref (userData) │
│  • lifecycle: restart on crash, kill on quit                               │
└───────────────┬────────────────────────────────────────────────────────────┘
                │ preload (contextIsolation, sandbox:true): baseUrl + OS IPC
                ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ RENDERER  (React 19 + TanStack, maximized)                                 │
│  • TanStack Query  → data layer over HTTP to the local Flue server         │
│  • TanStack Router → views (Chat / Session / Stacks / Settings)            │
│  • TanStack Store  → normalized live transcript + run state (SSE-fed)      │
│  • TanStack DB     → reactive collections (sessions, messages) [optional]  │
│  • TanStack Virtual→ transcript + session list virtualization              │
│  • TanStack Form   → settings / model picker / composer validation         │
│  • TanStack Table  → models + stacks tables                                │
│  • TanStack Pacer  → debounce/throttle composer + steering + queries       │
│  • TanStack Devtools→ Query + Router + (Store) devtools in dev             │
│  • SSE reader: GET /agents/nav/:id?live=sse → reduce into Store/Query      │
└───────────────┬────────────────────────────────────────────────────────────┘
                │ HTTP to 127.0.0.1:PORT (single origin)
                ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ FLUE BACKEND  (backend/  — Node target, built with flue build)             │
│  src/agents/nav.ts  → the coding agent (model + instructions + local()     │
│                       sandbox; built-in read/write/edit/grep/glob/bash)    │
│  src/app.ts         → Hono entry: flue() + nav control routes + startup    │
│                       line + /health                                       │
│  src/db.ts          → sqlite('./data/flue.db')  (Flue session persistence) │
│  src/nav/catalog.ts → nav session catalog (node:sqlite) — list/latest/...  │
│  src/nav/models.ts  → curated model catalog (static; no key needed)        │
│  src/nav/worktrees.ts → git worktree create/track for "worktree" mode      │
│  src/nav/routes.ts  → the /nav/* Hono router                               │
└──────────────────────────────────────────────────────────────────────────┘
```

**Data plane = direct renderer→Flue HTTP** (so TanStack Query/SSE are first-class).
**OS plane = IPC** (directory picker + mode pref must run in main; renderer can't).
A nav **session** *is* a Flue **agent instance** addressed by `id` (`POST/GET /agents/nav/:id`).

---

## 3. Key design decisions (decisive — do not re-litigate)

### 3.1 A nav session = a Flue addressable agent instance
The agent module `src/agents/nav.ts` is discovered as agent name **`nav`**. Each nav `sessionId`
becomes a Flue instance `id`. Sending a message is `POST /agents/nav/:id` (`{ message }`); the event
stream is `GET /agents/nav/:id?live=sse&offset=…`. This is Flue's documented continuing-conversation
model and fits nav's multi-turn sessions exactly. (Ref: `routing-api`, `sdk/agents`, `streaming-protocol`.)

The agent module **must export `route`** for these HTTP routes to be mounted:
```ts
export const route: AgentRouteHandler = async (_c, next) => next();
```
(Mounting `flue()` alone is not enough — see `guide/react` and `routing-api`.)

### 3.2 The coding toolset comes from the sandbox, not hand-wired tools
With `sandbox: local()` the agent gets built-in **read / write / edit / grep / glob / bash** in its
`cwd` (Ref: `ecosystem/deploy/node` "Using the local sandbox"; these are pi-agent-core's tools, which
Flue runs on). So the core coding agent is essentially:

```ts
// backend/src/agents/nav.ts
import { createAgent, type AgentRouteHandler } from '@flue/runtime';
import { local } from '@flue/runtime/node';
import { catalog } from '../nav/catalog.ts';
import { DEFAULT_MODEL } from '../nav/models.ts';

export const description = 'nav coding agent';
export const route: AgentRouteHandler = async (_c, next) => next();

export default createAgent(({ id }) => {
  const entry = catalog.get(id);            // per-session config (see §3.4)
  return {
    model: entry?.model ?? DEFAULT_MODEL,    // string specifier, e.g. 'anthropic/claude-sonnet-4-6'
    thinkingLevel: entry?.thinkingLevel ?? 'medium',
    instructions: NAV_SYSTEM_PROMPT,
    sandbox: local({ env: { /* expose only what bash genuinely needs */ } }),
    cwd: entry?.cwd ?? process.cwd(),        // workspace root (or worktree path)
  };
});
```

**Harvesting pi's 7 tools is optional polish (Phase 6), not required.** Do the built-in-sandbox path
first; it is the documented, supported route and needs zero tool glue.

### 3.3 Replace the bespoke JSON-RPC; keep only the startup line
The old `/rpc` + `/sessions/:id/events` wire format was shaped around the Rust backend. We are
rewriting the renderer, so we drop it. The renderer talks to **two route families on one origin**:
- **Flue native** (`/agents/nav/:id`, `/openapi.json`) — run + stream.
- **nav control-plane** (`/nav/*`) — everything Flue has no opinion about: the session *catalog*
  (list/latest/resume metadata), the curated *model* list, *stacks*, worktree creation, stop.

Keep printing `nav local backend listening on <url>` so `backend-process.cts` keeps working with a
one-line change (spawn `node` instead of `cargo`). The `nav startup trace ` stderr channel is optional
— leave it unused (the main process tolerates its absence).

### 3.4 Per-session model / thinking via the agent initializer + catalog
`createAgent(({ id }) => …)` re-runs on each interaction (Ref: `agent-api` — "Do not treat it as a
one-time constructor"). So the initializer can read the session's chosen model/thinking from the nav
**catalog** keyed by `id`. `switchModel`/`switchThinking` simply write the catalog; the next prompt
picks them up. No per-prompt model field is needed on the native route (it accepts only
`{ message, images? }`).

### 3.5 Session catalog is nav-owned (Flue has no instance-listing API)
Flue persists *conversation* state (`SessionStore`, keyed by an opaque storage key) but exposes **no
"list all agent instances"** API. `listAgents()` lists agent *modules*, not instances. So nav must
keep its own **catalog** of sessions for the sidebar/list/latest/resume:

```
catalog row: { sessionId, workspaceRoot, mode, worktreePath|null, model, thinkingLevel,
               title, createdAt, updatedAt }
```

Store it in a **separate** SQLite DB via Node 24's built-in `node:sqlite` (no native dependency).
Do **not** co-opt Flue's `db.ts` schema. `session.list`/`latest`/`resume` are pure catalog reads;
`session.create` inserts a row (and, for worktree mode, creates the worktree first).

`session.list` must return what the sidebar consumes — `SessionSummary[]` (from
`renderer/src/types.ts:30`): `{ sessionId, title, workspaceRoot, projectRoot, updatedAt }`.
`projectRoot` is the checkout root; `workspaceRoot` is the agent `cwd` (these differ in worktree
mode). `session.latest { cwd }` returns the newest row whose `projectRoot`/`workspaceRoot` matches.
Derive `title` from the first user message (store/update it on first send) or fall back to the
workspace basename.

> Verify: `node:sqlite` is available and stable enough on the pinned Node 24. If it is flagged
> experimental and noisy, fall back to a small JSON file with atomic writes (the catalog is tiny and
> single-process). Decide in Phase 2; record the choice in the backend README.

### 3.6 Worktree mode lives in the backend now
The Rust backend + `project-session.cts` owned worktree creation. Move it to the backend
(`src/nav/worktrees.ts`): on `session.create` with `mode:"worktree"`, run `git worktree add` under
`.nav/worktrees/<id>/` off the workspace root, store `worktreePath` in the catalog, and set the
agent's `cwd` to it. **MVP:** implement `local` mode fully first; land `worktree` mode behind the
same catalog shape immediately after (it is just "compute a different cwd"). The renderer's "Start in
local/worktree" control maps to the `mode` field on create.

### 3.7 Model catalog is static and key-free
`session.models` returns a curated list the UI can show. It does **not** require a provider key — it
is just specifier metadata. The renderer's `ModelOption` (`types.ts:38`) is
`{ provider, model, label, thinkingLevels[] }`, so include per-model `thinkingLevels`. Define it in
`src/nav/models.ts`:
```ts
export const DEFAULT_MODEL = 'anthropic/claude-sonnet-4-6';
const ALL_THINKING = ['off','minimal','low','medium','high','xhigh'] as const;
export const MODELS = [
  { provider: 'anthropic', model: 'claude-sonnet-4-6', label: 'Claude Sonnet 4.6',
    contextWindow: 200_000, thinkingLevels: ALL_THINKING },
  { provider: 'anthropic', model: 'claude-opus-4-8',   label: 'Claude Opus 4.8',
    contextWindow: 200_000, thinkingLevels: ALL_THINKING },
  // …add the providers/models you actually configure; keep provider/model valid Flue specifiers
];
```
`modelInfo(sessionId)` returns the renderer's `ModelInfo` (`types.ts:50`):
`{ label, provider?, model?, thinking?, thinkingLevels[], tokenUsage? }`. `tokenUsage` is
`{ used, contextWindow }` (`TokenUsage`, `types.ts:45`). Build it from the catalog's selected
model + thinking; populate `tokenUsage.used` from the latest run's usage once a real run has
happened (Flue `PromptResponse.usage` / `turn` events; before any run, omit `used` or report 0).
This ties into the existing TokenBudgetGuard surface (`contextWindow` came from there).

### 3.8 What we deliberately do NOT do (now)
- No reimplementation of JSON-RPC 2.0.
- No custom Flue persistence adapter (use built-in `sqlite()`).
- No remote sandbox (use `local()`; nav is a trusted single-user dev tool on the host).
- No `@flue/react` `useFlueAgent` — we are "maximizing TanStack," so the Store/Query layer owns state
  and a thin SSE reader feeds it. (`@flue/sdk` may still be used purely as the HTTP/stream transport
  inside Query functions — see §5.3. Do not pull in `@flue/react`.)
- No attempt to make `session.stop` a guaranteed server-side kill until §8's verification resolves
  whether Flue exposes mid-turn cancellation.

### 3.9 "Stacks" = captured model turns, gathered via `observe()` server-side
nav's "stacks" are a debug view of the **raw model API calls** a session made. The renderer types
(`types.ts:59-91`) are: `StackEntry { id, runId, sequence, status, startedAtMs, durationMs,
request?, response? }`, `StackRequest { api, url, model, body? }`, `StackResponse { statusCode?,
body?, error?, tokenUsage? }`, `SessionStacksResult { stacks[], unavailableReason? }`,
`StackAvailabilityResult { available }`.

In Flue this maps to the **`turn_request` / `turn`** events (provider, model, model-visible input,
tools, usage). **Critical constraint:** `turn_request` is **in-process only — never persisted to
durable streams and never served over HTTP** (Ref: `events-reference` "Model turns"). So stacks
**cannot** be reconstructed from the agent's HTTP event stream. To implement `session.stacks`
faithfully, the backend must subscribe with `observe()` (in-process), capture `turn_request`/`turn`
(and their `turnId`/`operationId`/session correlation) per session, and store them in a nav stacks
store (a table in the catalog DB). `session.stacks` then reads that store;
`session.stackAvailability` reports whether retention is on / any rows exist.

- Apply an export-local **sanitization** policy before storing (events can carry secrets/prompts) —
  the events-reference explicitly warns about this.
- The capture→store plumbing is **offline-testable** (feed synthetic `turn_request`/`turn` objects to
  the observer and assert stored `StackEntry` rows). The *content* needs a real run (deferred, §11).
- MVP option: ship `session.stacks` returning `{ stacks: [], unavailableReason: 'capture not yet
  enabled' }` and `stackAvailability { available:false }` so the UI degrades cleanly, then land the
  `observe()` capture. Decide in Phase 2; don't block the phase on full capture.

---

## 4. Repository & workspace layout

Convert the repo to an explicit pnpm workspace with the backend as a second package. The renderer
stays a Vite root that shares root-hoisted deps (as today).

```
nav/
├─ package.json                ← Electron app + renderer deps (root); scripts orchestrate all 3
├─ pnpm-workspace.yaml         ← add  packages: ["." , "backend"]
├─ .nvmrc                      ← 24.16.0 (unchanged)
├─ tsconfig.json               ← renderer + tests typecheck (unchanged base)
├─ tsconfig.main.json          ← main/preload → out/*.cjs (unchanged)
├─ biome.json                  ← extend includes to cover backend/** (see Phase 0)
├─ desktop/electron/           ← main, preload, renderer (renderer rewritten in Phase 5)
├─ backend/                    ← NEW Flue project (its own package.json, tsconfig, flue.config.ts)
│  ├─ package.json             ← @flue/runtime, valibot, (dev) @flue/cli, typescript
│  ├─ flue.config.ts
│  ├─ src/{app.ts, db.ts, agents/nav.ts, nav/*}
│  └─ dist/                    ← flue build --target node → server.mjs (gitignored)
├─ tests/                      ← Electron node:test suites (extend; backend has its own vitest)
└─ docs/                       ← this plan + handoff
```

Add to `pnpm-workspace.yaml`:
```yaml
packages:
  - "."
  - "backend"
```
(Keep the existing `allowBuilds` + `engineStrict`.) `allowBuilds` may need `better-sqlite3`/native
entries **only** if you reject `node:sqlite` in §3.5 — otherwise leave as is.

---

## 5. The renderer: maximize TanStack

> Detailed component-by-component mapping is in §5.4 and the library matrix in §6. This section sets
> the architecture; §5.4 lists the concrete files. (Renderer reconnaissance is being finalized; treat
> the component responsibilities below as the contract and adjust names to the actual files in
> `desktop/electron/renderer/src/`.)

### 5.1 Dependencies (add to **root** `package.json` deps; renderer imports them via Vite)
```
@tanstack/react-query          @tanstack/react-query-devtools
@tanstack/react-router          @tanstack/router-devtools     (or @tanstack/react-router-devtools)
@tanstack/react-store           @tanstack/store
@tanstack/react-virtual
@tanstack/react-form
@tanstack/react-table
@tanstack/react-pacer           (debounce/throttle/rate-limit primitives)
@tanstack/react-db             (optional — reactive collections; adopt if it earns its place)
```
Pin to the latest stable majors; **verify current package names + peer ranges** with the TanStack
docs/npm before adding (names like `@tanstack/react-pacer` and `@tanstack/react-db` are newer — confirm
they exist at the version you install). Do not add `@tanstack/react-start` (SSR/full-stack; N/A in
Electron). Do not add `@tanstack/react-ranger` unless a real slider UI appears.

### 5.2 Where the backend URL comes from
The Flue server binds an ephemeral port (`127.0.0.1:0`). Main resolves the URL from the startup line
and must hand it to the renderer. Implement:
- preload exposes `window.nav.getBackendUrl(): Promise<string>` (IPC `nav:get-backend-url`) **and**
  keeps the existing `onBackendStatus` event (it already carries `backendUrl`).
- The renderer bootstraps by awaiting `getBackendUrl()` before constructing the Query client / SSE
  reader. Until it resolves, render a "connecting to backend" state (drive it off `onBackendStatus`).

### 5.3 Transport inside TanStack
- **Queries/mutations:** plain `fetch` (or `@flue/sdk` `createFlueClient({ baseUrl })`) wrapped in
  Query `queryFn`/`mutationFn`. `@flue/sdk` is fine *as a transport* (`client.agents.send`,
  `client.agents.stream`); it is not `@flue/react`. Decide per-call: native agent routes → SDK is
  convenient; `/nav/*` routes → plain `fetch` JSON.
- **Live events (SSE):** open `GET /agents/nav/:id?live=sse&offset=<resume>` (or `client.agents.stream`).
  Parse `event: data` frames (JSON **array** of Flue events) and `event: control` frames (track
  `streamNextOffset` for resume); ignore `: heartbeat`. Feed each Flue event through the §7 reducer
  into a **TanStack Store** transcript slice. Reconnect from the last `streamNextOffset` on drop.
  (Ref: `streaming-protocol` "SSE framing".)

### 5.4 Renderer structure (target)
```
renderer/src/
├─ main.tsx              ← Query client + Router + Store providers; bootstrap baseUrl; Devtools (dev)
├─ router.tsx            ← TanStack Router: routes for /, /session/$id, /session/$id/stacks, /settings
├─ api/
│  ├─ client.ts          ← baseUrl-bound fetch / @flue/sdk client
│  ├─ sessions.ts        ← Query hooks: list/latest/create/resume/stop  (→ /nav/*)
│  ├─ models.ts          ← Query hooks: models/modelInfo/switchModel/switchThinking
│  ├─ stacks.ts          ← Query hooks: stacks/stackAvailability
│  └─ stream.ts          ← SSE reader → Store dispatch (§7)
├─ store/
│  ├─ transcript.ts      ← TanStack Store: per-session messages, tool calls, run status
│  └─ selectors.ts       ← derived state (active run, streaming flag, token/usage)
├─ routes/ (or pages/)
│  ├─ ChatView.tsx       ← composer + transcript for the active session
│  ├─ StacksView.tsx     ← stacks table (TanStack Table)
│  └─ SettingsView.tsx   ← model picker + "Start in" mode (TanStack Form)
└─ components/
   ├─ Composer.tsx       ← TanStack Form + Pacer (debounced draft, throttled steering); sendMessage mutation
   ├─ Transcript.tsx     ← TanStack Virtual over messages; renders markdown (keep marked + dompurify)
   ├─ Sidebar.tsx        ← session list (Virtual if long); switch/new/add-project
   ├─ ModelsTable.tsx    ← TanStack Table over MODELS; row action = switchModel
   └─ StacksTable.tsx    ← TanStack Table over stacks
```

The existing renderer keeps `marked` + `dompurify` for message rendering — retain them. Keep the CSS
under `renderer/styles/` (reuse/adjust; not a rewrite target).

### 5.5 Optimistic send + reconciliation
On send: a `useMutation` posts to `POST /agents/nav/:id`, immediately appends an optimistic user
message to the Store, then ensures the SSE reader is reading from the returned `offset`. The stream
reconciles the optimistic message with durable events and drives run status to completion/idle.
(Mirror the behavior described in `guide/react` "sendMessage adds the user message immediately.")

### 5.6 Query invalidation replaces the manual refresh-on-event web
Today `App.tsx` hand-fires refreshes from the SSE handler: on `tool.*`/`message.completed` it
re-calls `modelInfo`/`stacks`; on terminal events it re-calls `list`/`modelInfo`/`stacks` (twice,
with a 120ms delay to catch async stack append). Replace this with **`queryClient.invalidateQueries`**
driven by the §7 reducer: when the reducer commits a tool/message event invalidate
`['modelInfo', id]` + `['stacks', id]`; on a terminal event invalidate `['sessions']` + `['modelInfo',
id]` + `['stacks', id]`. Query's `staleTime`/dedup subsumes the manual counters
(`stackRequest`/`modelInfoRequest` in `App.tsx:44`). Keep a single short delayed invalidation for
stacks if the capture store appends slightly after the terminal event.

---

## 6. TanStack library usage matrix (maximize coverage, but each must earn its place)

| Library | nav use | Notes |
| --- | --- | --- |
| **Query** | All RPC-equivalent reads/writes: sessions, models, modelInfo, stacks, stackAvailability; send/stop mutations | Core data layer. Cache keys per session id. |
| **Router** | Views: chat, per-session, stacks, settings; deep-link the active session id | Memory history (Electron file://) — verify Router's history works under `file://`; use the in-memory/hash history if needed. |
| **Store** | Live transcript + run state fed by SSE; cheap, fine-grained subscriptions | The reducer in §7 writes here. |
| **DB** | *Optional* reactive collections for sessions + messages, with live queries | Adopt only if it simplifies cross-component reads vs Store; otherwise skip — don't add ceremony. |
| **Virtual** | Transcript message list; sidebar session list when long | Biggest perf win; transcripts grow unbounded. |
| **Form** | Settings (model/thinking), "Start in" mode, composer validation | Validate non-empty message, valid thinking level. |
| **Table** | Models picker table; stacks table | Sorting/filtering models; columns for stacks. |
| **Pacer** | Debounce composer draft persistence; throttle steering/typing-driven queries; rate-limit reconnect | Replaces hand-rolled debounce. |
| **Devtools** | Query + Router (+ Store if supported) in dev builds only | Gate behind `import.meta.env.DEV`. |
| **Ranger** | Not used unless a range-slider UI is introduced | Skip. |
| **Pacer/Config/Start** | Start = N/A (SSR). Config = not needed. | Skip Start/Config. |

> "Maximize" means **use every library where it genuinely fits** (the table above), not bolt libraries
> on for their own sake. If a library has no honest use, say so in the PR rather than forcing it.

---

## 7. Event mapping: Flue native events → nav transcript model

> The renderer consumes Flue's **native** event union directly (no JSON-RPC translation layer). This
> reducer is a **pure function** — unit-test it with fixture arrays of Flue events; no model key
> needed. (Ref: `events-reference`.)

**Reuse what exists.** The renderer already has pure reducers and a normalized transcript model:
`reduceSessionState(state, event)` and `reduceSessions(states, event)` (`lib/session-runtime.ts:114`,
`:173`), over `SessionState { messages, running, stopPending, modelInfo, stackAvailable,
stackRefreshKey, messageSeq }` (`types.ts:114`) and `Message = ChatMessage | ToolMessage`
(`types.ts:95`). **Keep this model and its tests** — it is exactly the Store shape we want. The only
change is the *input*: today they switch on nav's `SessionEvent.type` (`user.message`, `tool.started`,
…); adapt them to switch on Flue's native event `type` per the table below (or put a thin
`flueEventToNavEvent(e)` adapter in front so the proven reducer body is untouched — preferred, since
it preserves the existing `tool_call_id`/`event_id` dedup and `messageSeq` logic). Either way the
reducer stays pure and the existing `tests/electron_session_runtime.test.cts` style carries over.

Map the stable Flue events to the renderer's transcript/run state. The legacy nav event names are
shown for orientation; the renderer no longer needs them as wire types.

| Flue event | Reducer effect | (legacy nav name) |
| --- | --- | --- |
| stream opened (HTTP 200 / first read) | mark session connected | `connected` |
| `message_start` (role=user) / optimistic | ensure user message present | `user.message` |
| `operation_start` (prompt) / `agent_start` | run begins; set status=running | `run.started` |
| `text_delta` | append streaming assistant text (best-effort) | (live progress) |
| `thinking_start/_delta/_end` | optional reasoning display | (live progress) |
| `message_end` (role=assistant) | commit authoritative assistant message | `message.completed` |
| `tool_start` | add tool-call entry (name + args), status=running | `assistant.tool_calls` + `tool.started` |
| `tool` (ended, ok) | mark tool-call completed; attach result | `tool.completed` |
| `tool` (ended, error) | mark tool-call failed; attach error | `tool.failed` |
| `operation` (ended, ok) / `idle` | run complete; status=idle | `run.completed` |
| `operation`/`turn` (ended, error) | run failed; surface error | `run.failed` |
| abort/cancel (see §8) | run cancelled | `run.cancelled` |
| `compaction_start` / `compaction` | optional "context compacted" notice | — |
| `submission_settled` (failed, on recovery) | run failed after interruption | `run.failed` |

Notes:
- Use `toolCallId` to correlate `tool_start` ↔ `tool`.
- `eventIndex` restarts per prompt on agent streams — **do not** use it as a global offset; use the
  Durable Streams `streamNextOffset` from control frames for resume (Ref: `streaming-protocol`).
- The detailed message payloads (`message` on `message_end`, etc.) mirror pi-agent-core's
  `AgentMessage` and are **not stable pre-1.0** — branch defensively and keep the reducer tolerant of
  shape drift (the envelope `v:1` is your version signal).

---

## 8. `session.stop` — the one genuinely uncertain piece

Flue's public HTTP surface for agents is `POST` (send) + `GET`/`HEAD` (stream). There is **no
documented mid-turn cancel route**, and `client.agents.send` returns `202` while the durable
submission keeps running server-side; aborting the HTTP request does not stop the agent loop.

**Plan:**
1. Implement `POST /nav/sessions/:id/stop` as a route that does the best available thing and returns
   `{ stopped: boolean }` honestly.
2. **Verify during build** (do this before claiming stop works): `flue docs search "cancel"`,
   `"abort"`, `"signal"`; check whether the installed `@flue/runtime` / `@flue/sdk` exposes a
   submission-cancel API or whether `AbortSignal` on an in-process operation propagates. The
   `CallHandle.abort()` path exists for in-process `session.prompt()` — but addressable agents are
   driven by the runtime, so confirm whether app code can reach the handle.
3. **Renderer-side stop always works:** stop the SSE subscription, freeze the optimistic UI, and mark
   the run `cancelled` locally. This is a real UX stop even if the server keeps finishing the turn.
4. Document the resolved behavior in the backend README. Do **not** fabricate a cancel API.

This honors the "never recommend without certainty" rule: ship the honest partial, flag the unknown,
resolve it against the live package — don't guess.

---

## 9. Phase plan (do in order; each ends with an offline gate)

### Phase 0 — Workspace + tooling scaffolding
**Goal:** repo is a clean 2-package pnpm workspace; gates run.
- Add `packages: [".", "backend"]` to `pnpm-workspace.yaml`.
- Create `backend/` skeleton (empty `src/`, `package.json` with `type: module`, `flue.config.ts`).
- Extend `biome.json` `files.includes` to cover `backend/**` (and keep ignoring `backend/dist`).
- Add root scripts that orchestrate the backend (e.g. `backend:build`, `backend:dev`, `backend:typecheck`,
  and fold backend build into `electron:dev`). Add `backend/dist` + `backend/data` + `backend/.env`
  to `.gitignore`.
**Gate:** `pnpm install` clean; `pnpm run typecheck` (renderer/main) still passes; `pnpm run lint`
clean.

### Phase 1 — Flue backend: agent + sandbox + persistence + server entry
**Goal:** a Flue Node server that boots, serves `flue()` + `/health`, prints the startup line, and
defines the `nav` agent. (No real model calls.)
- `backend/package.json`: deps `@flue/runtime`, `valibot`; dev `@flue/cli`, `typescript`,
  `vitest`. Add scripts `build` (`flue build --target node`), `dev` (`flue dev --target node`),
  `typecheck` (`tsc --noEmit`).
- `backend/src/db.ts`: `export default sqlite('./data/flue.db')`.
- `backend/src/agents/nav.ts`: as §3.2 (exports `route`, `description`, default `createAgent`).
  Use a placeholder `model: DEFAULT_MODEL` and the system prompt constant.
- `backend/src/app.ts`: Hono app — mount `flue()`, add `GET /health`, and a startup hook that prints
  `nav local backend listening on <url>` to **stdout** once listening. (For `flue build --target
  node`, the generated `dist/server.mjs` listens on `PORT`; bind `127.0.0.1` + an ephemeral port and
  print the resolved address. Verify how the generated Node entry exposes the bound address — see
  `ecosystem/deploy/node` + `guide/targets/node`; if the built server owns `listen()`, print the line
  from an app-level "ready" path or wrap the entry. Worst case, run with a fixed `PORT` from main and
  print that.)
**Gate:** `cd backend && pnpm run typecheck` passes; `pnpm run build` produces `dist/server.mjs`;
starting it (no key) prints the startup line and `GET /health` + `GET /openapi.json` respond. (A
prompt will fail without a key — that's expected and **out of scope** for the gate.)

### Phase 2 — nav control-plane (catalog, models, stacks, worktrees, stop)
**Goal:** all 12 RPC-equivalents exist as `/nav/*` routes (+ Flue native for send/stream), backed by
the catalog. Everything testable offline.
- `backend/src/nav/catalog.ts`: `node:sqlite` (or JSON fallback, §3.5) with `get/list/latest/insert/
  update/delete`. Pure, synchronous, well-typed.
- `backend/src/nav/models.ts`: `DEFAULT_MODEL` + `MODELS` (§3.7) + `modelInfo(entry)` helper.
- `backend/src/nav/worktrees.ts`: `createWorktree(workspaceRoot, id)` via `git worktree add`;
  `localCwd(workspaceRoot)`. (`local` mode first; worktree immediately after.)
- `backend/src/nav/routes.ts`: Hono router for the table in §1.2. `session.sendMessage` is **not**
  here — it's the native `POST /agents/nav/:id`. `stop` per §8.
- `backend/src/nav/stacks.ts`: the `observe()` capture + store per §3.9. **MVP-acceptable** to ship
  the stub (`{ stacks: [], unavailableReason }` / `available:false`) and land capture after; the
  store schema + the capture reducer should still be written and unit-tested with synthetic events.
- Mount in `app.ts`: `app.route('/nav', navRoutes)`. Register the `observe()` subscriber at server
  start (in `app.ts`).
- **Vitest** suites for: catalog CRUD + latest-by-cwd (returns `SessionSummary`); model catalog shape
  (incl. `thinkingLevels`); `modelInfo` builder (incl. `tokenUsage`); worktree path computation
  (mock `git`); stacks capture reducer (synthetic `turn_request`/`turn` → `StackEntry`); route
  handlers with a stubbed catalog (via Hono `app.request`).
**Gate:** `cd backend && pnpm run typecheck && pnpm exec vitest run` green; manual `curl` of
`/nav/sessions` (create→list→latest→resume→delete) works against a running server.

### Phase 3 — Repoint Electron main → Flue backend
**Goal:** the app launches the Flue server and the renderer can reach it; OS IPC trimmed to essentials.
- `backend-process.cts`: change `spawn("cargo", [...])` → spawn Node on the built server, e.g.
  `spawn(process.execPath, [path.join(PROJECT_ROOT, "backend/dist/server.mjs")], { cwd, env:{ ...process.env, PORT:"0", ...env } })`.
  Keep the `STARTUP_PREFIX` watch unchanged. (Decide dev vs prod: dev may use `flue dev`; prod uses the
  built `server.mjs`. Keep one code path for the overnight build — the built server.)
- `main.cts`: stop using `sendRpc`/`subscribeToSessionEvents` for the data plane. Keep: backend
  spawn/lifecycle, `nav:backend-status` (include `backendUrl`), the **directory picker**
  (`nav:create-project` → return chosen dir only; session creation moves to the renderer mutation),
  and the **mode pref** (`nav:get/set-session-mode`). Add `nav:get-backend-url`.
- `preload.cts`: prune to the OS surface (`getBackendUrl`, `onBackendStatus`, `pickProjectDirectory`,
  `getSessionMode`, `setSessionMode`). Remove the RPC passthroughs (they move to renderer HTTP).
- Update/replace `tests/electron_backend_client.test.cts` (the 2 expected failures): either delete
  `backend-client.cts` (and its test) if the renderer no longer goes through main for RPC, or repoint
  the test to the new transport. Keep `request-validation` tests if validation still lives in preload.
**Gate:** `pnpm run typecheck` + `pnpm run main:build` + `node --test tests/*.test.cts` green (the
formerly-failing backend tests are now removed or repointed). App boots, window loads, renderer logs
the resolved backend URL. (No model turn required.)

### Phase 4 — SSE reader + event reducer (pure, tested)
**Goal:** the renderer can read a Flue agent stream and reduce it into transcript state — verified
with fixtures, offline.
- `renderer/src/api/stream.ts`: SSE frame parser (data array / control / heartbeat) + resume via
  `streamNextOffset`.
- `renderer/src/store/transcript.ts`: TanStack Store holding the **existing** `SessionState` model;
  feed it via the adapted `reduceSessionState`/`reduceSessions` + the `flueEventToNavEvent` adapter
  (§7). Reuse `lib/session-runtime.ts` rather than rewriting the reducer.
- Tests: keep the spirit of `tests/electron_session_runtime.test.cts`; add `flueEventToNavEvent` unit
  tests feeding **synthetic** Flue event arrays → asserting the adapter + reducer output. (Capture a
  real event array later from a manual keyed run to harden fixtures — optional, post-key.)
**Gate:** reducer + parser unit tests green. No live stream needed.

### Phase 5 — Renderer TanStack rewrite (the large phase)
**Goal:** the renderer in §5.4 with the §6 matrix. Break into sub-commits:
1. Providers + bootstrap (`main.tsx`, Query client, Router skeleton, baseUrl bootstrap, Devtools).
2. `api/*` Query hooks over `/nav/*` + native send.
3. Store + wire `stream.ts` into ChatView.
4. Composer (Form + Pacer + send mutation + optimistic, §5.5).
5. Transcript (Virtual + markdown).
6. Sidebar (sessions list; new/switch/add-project via picker IPC + create mutation).
7. ModelsTable + SettingsView (Table + Form; switchModel/switchThinking).
8. StacksView (Table; stacks + availability).
9. Remove dead plain-React data code (`lib/session-runtime.ts` etc.) once superseded.
**Gate (per sub-commit and at end):** `pnpm run renderer:build` + `pnpm run typecheck` + `pnpm run
lint` green; component/unit tests for hooks (mock fetch) and reducers green. Manual visual check of
each view rendering with **mocked** data (no model).

### Phase 6 — (Optional) harvest pi tools for richer tool UX
Only if the built-in sandbox tools prove insufficient (e.g., you want nav-specific tool result
rendering or the `operations` seam to route file ops differently). Follow the handoff doc
(`docs/handoff-ade-electron-backend.md` §4): copy the 7 tool modules, strip pi-tui/`modes/interactive`
imports + render fns, keep `execute` + schemas + `operations`, register via `createAgent({ tools })`.
**Gate:** backend typecheck + the agent still boots; tool definitions validate via `defineTool`.

### Phase 7 — Cleanup, docs, final gates
- Remove any remaining Rust/`cargo` references, dead files, and stale comments.
- Update `desktop/electron` comments that mention the Rust backend.
- Write `backend/README.md` (how to build/run, the catalog/db split, the §8 stop resolution, the model
  catalog, worktree behavior, and the env vars — incl. where `ANTHROPIC_API_KEY` goes for real runs).
- Update root `README`/docs to describe the Flue + TanStack architecture.
**Gate:** full green: `pnpm run check:electron`, `cd backend && pnpm run typecheck && pnpm exec vitest
run`, `pnpm run lint`. Tree builds end-to-end. Manual smoke (§11) listed for the human.

---

## 10. Testing strategy (offline — no model API key)

**Testable without a key (must stay green in the overnight run):**
- Backend: typecheck, `flue build`, server boots + `/health` + `/openapi.json`, `/nav/*` route
  handlers (Hono `app.request`), catalog CRUD, model catalog shape, worktree path logic (mock git),
  stream-route 404/contract behavior.
- Renderer: typecheck, `vite build`, the **SSE parser** + **event reducer** (fixture arrays), Query
  hooks with mocked `fetch`, component render tests with mocked data, Form validation, Table
  sorting/filtering.
- Electron main: existing `node:test` suites (window security, preload boundary, external links,
  request validation, session-mode store, project-list) — keep green; repoint/remove the 2 backend
  tests.

**NOT in scope (needs a real model — defer to manual §11):**
- An actual prompt → tool calls → assistant response round-trip.
- Real Flue event shapes end-to-end (use synthetic fixtures; harden later from a captured run).
- `session.stop` killing a real in-flight turn (depends on §8 resolution).

Do **not** add tests that require a key, and do **not** gate any phase on them.

---

## 11. Manual smoke checklist (for a human, with a real key)
1. Put `ANTHROPIC_API_KEY` (or your provider's key) in `backend/.env`.
2. `cd backend && pnpm run build && node dist/server.mjs` → confirm a prompt via
   `curl -XPOST 127.0.0.1:PORT/agents/nav/test -d '{"message":"list files"}'` streams tool + text events.
3. Launch the Electron app; create a session, send a message, watch the transcript stream, switch
   model, open stacks, stop a run, resume on relaunch.
4. Capture a real event array from step 2/3 and fold it into the reducer fixtures (§Phase 4) to harden
   tests.

---

## 12. Conventions & guardrails for the overnight agent
- **Commits:** one per phase/sub-commit, human voice, imperative subject. **No** "Co-Authored-By",
  **no** "Generated with Claude Code", no AI attribution (per user global rules). Keep the tree green
  at every commit.
- **Don't** use bun/npm; pnpm only. **Don't** add deps without pinning + a one-line justification.
- **Verify fast-moving APIs** before coding against them: Flue (`flue docs`/`deepwiki`), TanStack
  (current package names + peer deps), Node 24 `node:sqlite` stability. This plan is a map, not a spec
  — the installed packages win.
- **When blocked or uncertain** (e.g., §8 cancel, the server `listen()`/startup-line seam, Router under
  `file://`): implement the honest partial, leave a clearly-marked `TODO(verify): …` with the exact
  doc query to run, and keep going. Never fabricate an API to make a gate pass.
- **Scope discipline:** the goal is a working Flue+TanStack nav, not a rewrite of everything. Reuse
  CSS, markdown rendering, window-security, and OS-IPC code that already works.

---

## 13. Reference index
- Flue agent API / sessions / tools: `flue docs read api/agent-api`, `guide/tools`, `guide/building-agents`.
- Flue HTTP + streaming: `flue docs read api/routing-api`, `api/streaming-protocol`, `sdk/agents`.
- Flue events: `flue docs read api/events-reference`.
- Flue persistence / sandbox / models: `flue docs read guide/database`, `guide/sandboxes`, `guide/models`,
  `ecosystem/deploy/node`, `api/data-persistence-api`.
- pi tool harvest details: `docs/handoff-ade-electron-backend.md`.
- Current Electron contract: `desktop/electron/{main,preload,backend-process,backend-client}.cts`.
