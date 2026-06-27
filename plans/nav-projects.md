# Nav Projects (multi-app) + sidebar

Status: implemented in this working tree from the finalized plan (co-designed with Codex gpt-5.5, 2026-06-28).
Builds directly on the shipped sessions work (`plans/nav-sessions.md`), which deliberately
deferred projects and left a nullable `nav_sessions.project_id` column reserved for this.

## Goal

Let Nav work on **multiple apps**, not just its own repo. Add a **Projects** list to the sidebar
where each project is a folder/app; selecting a project scopes the agent's working directory to it.
Each project owns many **sessions** (chats); show up to **5 recent sessions** per project, the rest
behind **"Show more"**. Mirror the Codex desktop sidebar (user-provided reference screenshot).

## Problem (current state)

- The Nav agent's cwd is fixed. `agents/nav.ts` does `sandbox: local({ cwd: getWorkspaceRoot() })`
  where `getWorkspaceRoot()` walks up to the nav repo's `pnpm-workspace.yaml`. Every chat runs in
  the nav repo. There is no per-app routing.
- The sidebar (post-sessions) is a single flat **"Chats"** group. There is no project taxonomy.
- Instructions hardcode "Nav monorepo at <root>", which is wrong for any other app.

## Constraints (verified by reading the repo + installed runtime)

1. **Per-instance cwd hook exists and is proven.** `defineAgent((ctx) => config)` receives
   `AgentInitializerContext = { id, env }` where `id` is the agent instance id (= the session id
   passed to `useFlueAgent({ id })`). The factory may be sync or async. `agents/glm.ts` already
   derives a per-instance cwd this way: `local({ cwd: createAgentWorktree("glm", ctx.id) })`. So the
   Nav factory can resolve cwd **per session** from `ctx.id`.
2. **`node:sqlite` is synchronous.** `nav-sessions.ts` already reads `flue.db` via a `DatabaseSync`
   singleton in-process. A cwd lookup in the factory is a cheap sync query — no async needed.
3. **`nav_sessions.project_id` already exists** (nullable, no UI today). We bind sessions to
   projects through it; no migration to add the column.
4. **The list already hides transcript-less rows.** `listNavSessions` skips any `nav_sessions` row
   whose `flue_sessions` transcript doesn't exist yet. => we can safely create a session row at
   draft time (to carry `project_id`) without cluttering the sidebar with empty chats.
5. **`ToolContext` is `{ signal, emitData, input }` only.** A tool can NOT read its caller's session
   id or cwd. The current `consult`/`consult_panel` tools are process-global (env + `getWorkspaceRoot`),
   which is why making the fleet project-aware is non-trivial — and why we gate it out of v1 (below).
6. **Native folder picker needs Electron.** The renderer is sandboxed (`contextIsolation`). Adding a
   "choose folder" dialog requires one new `ipcMain.handle` (`dialog.showOpenDialog`) + a preload
   method on `window.navDesktop`. (Today preload exposes only `getFlueConnection` / `onFlueStatus`.)
7. The Flue server runs from `packages/flue`; `getWorkspaceRoot()` (or `NAV_CODEX_WORKDIR`) is the
   nav repo root and is the natural **default project** path.
8. Repo rules (`AGENTS.md`): **don't over-engineer**; prefer UUID v7; don't hand-edit shadcn
   components under `components/ui` / `components/ai-elements`.

## Architecture decision

**Flue owns a new `nav_projects` table in the same `flue.db`. A project is a directory. Sessions
bind to a project via `nav_sessions.project_id`. The Nav agent resolves its sandbox cwd per-session
from that binding, inside the `defineAgent` factory — the same seam `glm.ts` uses for worktrees.**

Rejected / explicitly out of v1:
- **Per-project fleet.** Because `ToolContext` can't see the caller, a delegate would snapshot the
  nav repo even while Nav is in another app — a wrong-repo correctness bug. Rather than build the
  closure+map+nearest-git-root machinery now, **v1 gates the fleet to the default (nav) project**:
  the factory includes `consult`/`consult_panel` only when the resolved project is the nav repo.
  In every other project Nav is a correct solo agent. Project-aware fleet is the next milestone.
- **Cascade-deleting transcripts when removing a project.** Too destructive. "Remove project"
  **archives** (see below). Per-session hard delete (already shipped) stays for individual chats.

### Data model — `nav_projects` (new table in `flue.db`)

```
id            TEXT PRIMARY KEY     -- uuidv7
name          TEXT NOT NULL        -- display name; defaults to basename(path); user-editable
path          TEXT NOT NULL        -- canonical/realpath; the agent cwd. UNIQUE.
display_path  TEXT                 -- optional original (pre-realpath) path for display
is_default    INTEGER NOT NULL DEFAULT 0  -- the seeded nav repo; not removable
archived      INTEGER NOT NULL DEFAULT 0
created_at    INTEGER NOT NULL
last_opened_at INTEGER
```

- **Dedupe by canonical realpath**, not the raw string (`fs.realpathSync` + normalize; macOS is
  case-insensitive, so also case-fold for the uniqueness check). Re-adding an archived path
  **un-archives** the existing row — never creates a duplicate.
- `nav_sessions.project_id` is the FK (already present, nullable). NULL = legacy/default.

### Boot init (idempotent, in `packages/flue/.flue/app.ts`)

Add `ensureNavProjectsReady()` alongside the existing `ensureNavSessionsReady()`:
1. `CREATE TABLE IF NOT EXISTS nav_projects ...`.
2. **Seed the default project** for `getWorkspaceRoot()` if absent (`is_default = 1`,
   `name = basename(root)`).
3. **Backfill** legacy sessions: set `project_id = <default id>` where `project_id IS NULL`.
   (Idempotent; transcript-less rows untouched.)

Order matters: projects table + default seed must run before binding new sessions. Both
`ensureNavSessionsReady` and `ensureNavProjectsReady` are awaited/guarded the same way at boot.

### Agent cwd routing (in `agents/nav.ts` + a shared resolver)

```ts
export default defineAgent((ctx) => {
  const project = resolveSessionProject(ctx.id); // sync sqlite read; null for legacy/no-binding
  const cwd = project?.path ?? getWorkspaceRoot();
  const isDefault = !project || project.isDefault; // path === workspace root
  return {
    instructions: buildNavInstructions(cwd, { fleet: isDefault }), // genericized text
    model: "openai-codex/gpt-5.5",
    sandbox: local({ cwd }),
    tools: isDefault ? [consult, consultPanel] : [],
    thinkingLevel: resolveThinkingLevel(),
  };
});
```

- **Fail closed on cwd:** never substitute a *different* project's path. Only a genuinely
  unbound session (legacy NULL / no row) falls back to the default root. If a bound project's
  path is missing on disk, keep that path (the agent's first command fails honestly with ENOENT) —
  do not silently redirect to the nav repo.
- `resolveSessionProject(id)` lives in a shared module that reuses the **same in-process
  `DatabaseSync` handle** as `nav-sessions.ts` (extract the `getDb()` accessor to a shared
  `nav-db.ts` so there's one handle, one writer). The factory only reads.
- **Instructions genericized:** "You are Nav, a coding assistant working in the project at
  `<cwd>`." Fleet/team wording included only when `fleet` is true.

### Draft → project binding (the tricky bit)

The factory needs the project **before the first message is admitted**. So binding happens at
draft creation, not after send:

- New chat in project P → renderer mints a uuidv7 and **awaits `POST /api/sessions { id,
  projectId: P }`** (title null) **before** enabling/sending. Row is invisible until a transcript
  exists (constraint #4), so no empty-chat clutter.
- First `sendMessage` sets the title (keep the existing create-on-admit / first-message-title path;
  it becomes an idempotent upsert since the row already exists).
- A draft's project is **frozen** once created (a session never moves between projects).
- Crash mid-send is still recovered by the existing boot backfill (transcript is source of truth).

### Backend API (in `packages/flue/.flue/app.ts`, same auth tier as `/api/sessions`)

Keep `/api/projects` and `/api/sessions` **separate** (no combined `/api/workspace`):

- `GET    /api/projects` — non-archived projects, sorted `last_opened_at` desc (default project
  always present). Returns `{ id, name, path, isDefault, archived, lastOpenedAt }[]`.
- `POST   /api/projects` — `{ path, name? }`. Validate the dir exists and is a directory;
  canonicalize to realpath; dedupe (un-archive if the path was archived); `name` defaults to
  basename. Returns the project.
- `PATCH  /api/projects/:id` — `{ name?, archived? }` (rename / archive / un-archive).
- `DELETE /api/projects/:id` — **"Remove" = archive** (`archived = 1`); its sessions hide with it,
  transcripts preserved, re-adding the path restores. **The default project is not removable.**
- Extend sessions API: `GET /api/sessions` includes `projectId`; `POST /api/sessions` accepts
  `projectId` (legacy NULL still means default). Per-session hard delete is unchanged.

CORS already allows `PATCH`/`DELETE` (added in the sessions work) — no change.

### Electron IPC (folder picker)

- `main.ts`: `ipcMain.handle("dialog:pickProjectDirectory", ...)` → `dialog.showOpenDialog({
  properties: ["openDirectory"] })` → returns the selected path or `null` (canceled). Same
  trusted-sender check as `flue:getConnection`.
- `preload.cts`: expose `navDesktop.pickProjectDirectory(): Promise<string | null>`.
- Renderer "Add project" → picker → `POST /api/projects { path }` → refresh + select it.

### Renderer state (`packages/desktop`)

- Persist **`activeProjectId`** and **`activeSessionIdByProject`** (map projectId → last session id)
  in localStorage, replacing the single global `activeSessionId` (which would point across projects).
- Selecting a session sets **both** the session and its project. Switching projects restores that
  project's last session, or a fresh draft if none.
- "New chat" targets the **active project**. Add a per-project compose affordance on the active
  project header (matches the reference).
- Reuse the existing `sessions-client.ts` transport (direct authed HTTP); add a tiny
  `projects-client.ts` for the new endpoints. No new Electron IPC beyond the picker.

### Sidebar UI (match the Codex desktop reference)

- A **"Projects"** section. Each project is an **independently collapsible group**, **expanded by
  default**, collapse state remembered per project.
- The **active project** is highlighted and gets the extra affordances (chevron, "…" menu with
  rename/remove, compose/new-chat icon).
- Each expanded project shows up to **5 recent sessions** (by derived `updatedAt` desc) + a
  **"Show more"** toggle (client-side; the list already returns all sessions).
- Sessions show a **relative-time** label (e.g. `7h`, `4d`, `1w`) from derived `updatedAt`.
- A **missing-on-disk** project stays visible but marked unavailable; its old sessions remain
  viewable; New Chat/send is disabled or fails clearly; user can remove/archive or re-add later.

## Sequencing (final locked v1)

1. **`nav_projects` table** + boot init: create table, seed default project (nav repo,
   `is_default`), backfill `NULL` `nav_sessions.project_id` → default. Extract shared `getDb()`.
2. **Project APIs** — `GET/POST/PATCH/DELETE /api/projects` (realpath canonicalize + dedupe +
   un-archive; default project not removable; remove = archive).
3. **Sessions API** — `GET` returns `projectId`; `POST` accepts `projectId`. Per-session hard
   delete unchanged.
4. **Electron folder picker** — `navDesktop.pickProjectDirectory()` IPC.
5. **Renderer state** — persist `activeProjectId` + `activeSessionIdByProject`; selecting a session
   sets both; New Chat targets active project.
6. **Create-row-before-send binding** — await `POST /api/sessions { id, projectId }` before the
   first `sendMessage`; freeze draft's project.
7. **Agent cwd routing** — Nav factory resolves the session's project path; genericized
   instructions; legacy NULL → default root; fail closed on missing/invalid (never substitute a
   different project).
8. **Fleet gating** — keep `consult`/`consult_panel` only for the default project; omit for all
   others. No closure/map/worktree generalization yet.
9. **Sidebar UI** — independently collapsible project groups (expanded by default, remembered),
   active project highlighted with chevron/menu/compose, ≤5 sessions + "Show more", relative-time.
10. **Missing dirs** — visible-but-unavailable state; old sessions viewable; clear error on use.

## Out of scope (v1)

- **Project-aware fleet** (consult/consultPanel inside non-default projects). Requires resolving
  the caller's project despite `ToolContext` not exposing it (factory closure over projectRoot +
  delegationId→root map, fail-closed) and **nearest-git-root** worktrees (find the enclosing git
  root, snapshot there, run the delegate at the selected folder's relative subpath). Next milestone.
- Rich missing-dir recovery UX (re-locate flow), archived-projects browser (re-adding the path is
  the v1 restore path), per-project settings, drag-reorder, project colors/icons.
- LLM-generated titles (already deferred in the sessions plan).

## Risks to verify during implementation

- **Factory invocation timing/caching.** Confirm whether `defineAgent`'s factory runs once per
  instance (cached) or per request. Either is fine (a session is bound to one project), but it
  affects how a sync DB read in the factory behaves under load. Verify against the installed runtime.
- **Single sqlite handle.** Ensure `resolveSessionProject` reuses the existing `DatabaseSync`
  handle (one writer) rather than opening a second connection; WAL allows concurrent readers but a
  shared handle is cleaner. Extract `getDb()` to a shared module both files import.
- **realpath edge cases.** Symlinks, trailing slashes, macOS case-insensitivity, and the home dir.
  Canonicalize consistently for the uniqueness check; store a display variant separately.
- **Binding-before-send ordering.** Confirm `useFlueAgent`'s send path doesn't admit before the
  `POST /api/sessions` resolves; the renderer must await the bind first so the factory sees the
  project_id. Re-confirm the agent instance id (`useFlueAgent({ id })`) is exactly `ctx.id`.
- **Default-project detection for fleet gating.** Compare resolved project path to the canonical
  `getWorkspaceRoot()` (handles legacy NULL too) rather than relying solely on `is_default`.
