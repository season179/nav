# Nav Sessions + Sidebar

Status: implemented in this working tree from the finalized plan (co-designed with Codex gpt-5.5, 2026-06-27).

## Goal

Make Nav's chat have **proper persistent sessions** and **wire the sidebar** to them:
persist conversations, list them, switch between them, new chat, rename, delete, and
survive app restart. User ask: "App should start having proper sessions. Make use of
the sidebar."

## Problem (current state)

- `packages/desktop/src/main.tsx:219` mints `useState(() => createUuidV7())` per launch,
  so **every restart is a throwaway chat** with no way back.
- `packages/desktop/src/components/app-sidebar.tsx` is 100% placeholder: hardcoded
  `placeholderProjects` ("Project label" / "Chat label"), and "New chat" does nothing.

## Constraints (verified by reading the repo + installed runtime)

1. Flue already persists every conversation per agent-instance in `packages/flue/data/flue.db`:
   `flue_sessions` (JSON: conversationId, createdAt, metadata, leafId) keyed by the literal
   string `agent-session:["<id>","default","default"]`, plus `flue_session_entries` (the
   messages). `useFlueAgent({name:"nav", id, history})` auto-hydrates the transcript over HTTP.
2. **The public Flue SDK has no list/enumerate API.** `FlueClient` only exposes
   `agents.{prompt,send,wait,stream}` (you must already know the id), `runs.*`, `workflows.*`.
   So **something must own the list of conversation ids** — Flue won't give it to us.
3. `@flue/react`'s `useFlueAgent` treats a **404 during history hydration as an empty dormant
   session**, not a fatal error (confirmed by reading the installed dist). => pointing it at a
   not-yet-materialized id is safe.
4. `@flue/runtime@1.0.0-beta.7` exports **no** high-level `deleteSession(id)`. Deletion must go
   through the adapter's execution store (it blocks new admissions, rejects queued/running work,
   and cleans journals/chunks/submissions — not just a row delete).
5. The Nav agent (`packages/flue/.flue/agents/nav.ts`) runs with sandbox cwd fixed to the repo
   root. **Multi-project (one project per working dir) is not wired anywhere.**
6. Repo rules (`AGENTS.md`): don't over-engineer; prefer UUID v7; don't hand-edit shadcn
   components under `components/ui` / `components/ai-elements`.

## Architecture decision

**Flue (our own app) owns a `nav_sessions` table in the same `flue.db`. Flue stays the source
of truth for transcripts; `nav_sessions` is the source of truth for the human list + org
metadata. Freshness (updatedAt / preview) is derived lazily from `flue.db` at list time.**

Rejected: a registry in the Electron **main** process (JSON or sqlite). It splits state across
two stores for no gain, forces the renderer/main to own freshness (the part most likely to
desync on a mid-stream crash), and adds an IPC surface. Co-locating the registry with `flue.db`
behind our Flue app means list reconciliation reads the transcript directly, so the list always
reflects what Flue actually persisted — the correct crash failure mode.

Coupling to Flue beta-internal table shapes (`agent-session:[...]` key, entry JSON) is accepted
**only inside one module in `packages/flue`**. We own and pin the beta version, so an upgrade is
a deliberate, single-file change.

### Data model — `nav_sessions` (new table in `flue.db`)

```
id            TEXT PRIMARY KEY    -- the id passed to useFlueAgent (uuidv7); 1st elem of the
                                  --   agent-session key. NOT Flue's internal conversationId.
agent_name    TEXT NOT NULL DEFAULT 'nav'
title         TEXT
title_source  TEXT NOT NULL DEFAULT 'first-message'  -- first-message | manual | imported | llm
pinned        INTEGER NOT NULL DEFAULT 0
archived      INTEGER NOT NULL DEFAULT 0
project_id    TEXT                -- nullable, reserved; no UI in v1
created_at    INTEGER NOT NULL
last_opened_at INTEGER
imported_at   INTEGER
```

`updatedAt` and `lastPreview` are **not stored** — derived per row at list time from
`flue_session_entries` (MAX position/timestamp + a short content preview).

`title_source` is a small state machine: a **manual** rename pins the title so future
first-message/LLM title logic never overwrites it.

### Backend API (in `packages/flue/.flue/app.ts`)

Mount **before** `app.route("/api", flue())`. Under `/api/*` (desktop-auth/bearer) but **not**
behind `requireCodexProvider` — the sidebar must open even if Codex auth is broken.

- `GET  /api/sessions` — list. `nav_sessions` (archived excluded by default) joined with derived
  freshness from `flue_session_entries`. Hide rows whose transcript no longer exists (cheap
  reconciliation). Sort: pinned desc, then derived updatedAt desc. Returns
  `{id, title, titleSource, pinned, archived, createdAt, updatedAt, lastPreview}[]`.
- `POST /api/sessions` — create/adopt `{id, title}`. `INSERT ... ON CONFLICT(id) DO NOTHING`.
  Called on first send.
- `PATCH /api/sessions/:id` — `{title?, pinned?, archived?}`. Setting `title` sets
  `title_source = 'manual'`.
- `DELETE /api/sessions/:id` — transcript-first, then hard-delete the row (see below).
- **Boot backfill** — idempotent import of existing non-empty Flue sessions (see below).

CORS (`app.ts:111`) currently allows only `GET/HEAD/POST/OPTIONS` — **add `PATCH` and `DELETE`**.

### Delete mechanism (transcript-first, hard delete, no tombstones)

Programmatic via the adapter's execution store — not a self-HTTP `DELETE /api/agents/nav/:id`
(that re-enters middleware and is gated by Codex auth):

```ts
const sessionKey = createSessionStorageKey(id, "default", "default");
await stores.executionStore.submissions.deleteSession(sessionKey, () =>
  stores.executionStore.sessions.delete(sessionKey),
);
// then: DELETE FROM nav_sessions WHERE id = ?
```

Ordering rationale: transcript-first + list reconciliation makes every failure recoverable and
needs no tombstones. (Row-first or a UNION auto-adopt list would require tombstones to stop
deleted chats resurrecting — avoided.) Deleting a streaming session fails clearly; the
submission store rejects queued/running work.

### Backfill (idempotent boot import)

On server boot, `INSERT ... ON CONFLICT DO NOTHING` a `nav_sessions` row for every **non-empty**
Flue session lacking one: title from the first user message, `title_source = 'imported'`. Skip
empty sessions. Never overwrite existing titles. Starting clean would make "proper sessions"
look like data loss — the user already has dozens of real conversations in `flue.db`.

### Renderer (`packages/desktop`)

- Transport: **direct authed HTTP** from the renderer using `connection.baseUrl + token` (same
  as chat). No new Electron IPC — IPC would just duplicate transport with no added safety. Add a
  tiny `lib/sessions-client.ts` (raw fetch) since `FlueClient` has no sessions methods.
- App holds `activeSessionId`, persisted (e.g. localStorage). On launch restore last-active;
  if missing/deleted, fall back to newest non-archived session, else a fresh draft.
- Drop the per-launch uuid in `NavChat`. Selecting a sidebar row sets `activeSessionId`, which
  re-mounts `useFlueAgent({ name:"nav", id: activeSessionId })` → history hydrates from `flue.db`.
- **New chat = draft**: create an active draft id in the renderer but do **not** persist a
  `nav_sessions` row until the first message. Avoids the sidebar filling with empty chats.
- First send order: `sendMessage` first, then `POST /api/sessions {id, title:<first msg>}` after
  admission. If the app dies between, boot backfill recovers the row.
- Sidebar: list from `GET /api/sessions`; refresh after send/rename/delete (poll-on-focus or
  re-fetch after mutations — no live socket needed in v1). New chat / select / rename / delete.

### Sidebar scope — defer projects

The agent cwd is fixed, so a "Projects" tree would lie. v1: rename the group to **Chats** or use
**recency groups** (Today / Yesterday / Previous 7 days), driven by derived updatedAt. Keep the
nullable `project_id` column but ship **no project UI** until cwd/project routing is real.
"Make use of the sidebar" = usable session navigation, not preserving the placeholder taxonomy.

## Sequencing

1. Backend: `nav_sessions` table + `GET/POST/PATCH/DELETE /api/sessions` + list reconciliation +
   boot backfill. CORS PATCH/DELETE.
2. Renderer: `activeSessionId` state + restore-last; sidebar list + switch + new draft (drop the
   per-launch uuid).
3. Rename + delete in the sidebar.
4. Title from first user message (renderer sets on first send; backfill sets on import).
5. Later (out of scope here): LLM-generated titles; real per-cwd projects.

## Out of scope (v1)

- Real projects / per-working-dir routing.
- LLM-generated titles.
- Live push of list updates (re-fetch after mutations is enough).

## Risks to verify during implementation

- **Execution-store handle in `app.ts`.** `db.ts` exports `sqlite("./data/flue.db")`. Confirm how
  `app.ts` obtains a connected `stores.executionStore`: import the same `store` from `./db.js` and
  `await store.connect()` (verify it's idempotent / shares the underlying connection), vs the
  runtime already holding the live one. This is the one fuzzy area.
- **Raw SQL for `nav_sessions` + freshness.** Confirm whether the adapter exposes a usable
  query/db handle for our own table + reads of `flue_session_entries`. If not, open a second
  `node:sqlite` handle to the same `data/flue.db` (WAL allows concurrent readers/writers) for
  `nav_sessions`, and use the adapter **only** for the delete path.
- **`createSessionStorageKey` import.** Confirm export path from `@flue/runtime`.
- Confirm the agent-session key really is `[id, "default", "default"]` for desktop chats (matches
  current `flue.db` rows) so backfill/delete target the right key.
