# Nav Projects — deferred work (post-v1 milestones)

Status: **co-agreed by Claude + Codex gpt-5.5, 2026-06-28** (Codex verdict: "CONVERGED: agree with the
plan as written", AGREE on Q1–Q7 + M2 mechanism + milestone order). Continues `plans/nav-projects.md`
(v1 implemented in this working tree). Centerpiece is **M2 project-aware fleet** — the one real
architectural item v1 deferred. M3–M6 are smaller; the recommended cut line is below M2 + M6.

> Process note: both v1 and this deferred plan were co-designed with Codex and converged on the same
> principles (fail-closed, smallest-correct, don't over-engineer), grounded in direct verification of
> the installed runtime (see "Verified facts"). Codex's one promoted item: **canonical realpath /
> subpath containment is an M2 acceptance criterion, not just a check** (see M2 Q7 + Risks).

## Ground v1 establishes (don't re-litigate)

- `nav_projects` table; `nav_sessions.project_id`; default project = nav repo.
- Nav factory resolves per-session cwd: `resolveSessionProject(ctx.id)?.path ?? getWorkspaceRoot()`,
  genericized instructions, **fail-closed** (never substitute a *different* project's path).
- **Fleet gated to the default project**: `tools: isDefault ? [consult, consultPanel] : []`. v1 does
  NOT touch `worktrees.ts`; delegates (`glm` / `deepseek-pro` / `deepseek-flash`) are unchanged —
  each `defineAgent((ctx) => ({ sandbox: local({ cwd: createAgentWorktree(agent, ctx.id) }) }))`, and
  `createAgentWorktree` snapshots `getWorkspaceRoot()` (nav repo) only.

## Verified facts the M2 design hinges on

- `AgentRouteHandler = Hono MiddlewareHandler`. flue-app mounts `app.all("/agents/:name/:id", …)` and
  runs each agent's exported `route` via `runAttachedMiddleware(c, agent.route, () =>
  routeAgentRequest(...))`. So a delegate's `route(c, next)` sees the full context —
  `c.req.header(...)` AND `c.req.param("id")` — and `next()` is what triggers the factory.
  => a clean **request → factory bridge** (route runs first, then the factory).
- The consult loopback hits the **same flue process**. A module-level `Map` shared between a
  delegate's `route` (producer) and its factory (consumer) is in-process and ordered: route sets the
  entry before `next()`, the factory reads it during `next()`.
- `ToolContext` is `{ signal, emitData, input }` only (decided in v1) — a tool can't read its caller,
  which is exactly why we bridge the project root via an HTTP header on the loopback call rather than
  through tool context.
- `createWorkspaceSnapshot(repoRoot)` already takes a root and uses a unique temp `GIT_INDEX_FILE`
  per call (safe under concurrent `consult_panel` snapshots of the same repo).

---

## M2 — Project-aware fleet (the milestone)

**Goal:** `consult` / `consult_panel` work in **any git-backed project**, each delegation isolated in
its own worktree of *that* project, fail-closed so a delegate can never touch the wrong repo.

### Mechanism (header + route bridge + in-process map)

1. **Gate change.** The Nav factory already resolves `projectRoot`. Add a sync
   `resolveGitContext(projectRoot)`:
   - `gitRoot = git -C projectRoot rev-parse --show-toplevel` (fails ⇒ not a git repo).
   - `hasHead = git -C gitRoot rev-parse --verify HEAD` succeeds (≥1 commit).
   - `subpath = path.relative(gitRoot, projectRoot)`.
   Include the consult tools when **git-backed AND hasHead**; otherwise omit them (solo agent) and
   say so in one line of instructions (e.g. "fleet unavailable: project is not a git repo / has no
   commits yet"). This **replaces** v1's `isDefault` gate. The nav repo satisfies it, so the default
   project keeps today's behavior.

2. **Tools become factory closures.** Refactor `delegation.ts` from module-level `defineTool`
   singletons to `makeConsult(gitCtx)` / `makeConsultPanel(gitCtx)` that close over
   `{ gitRoot, subpath }`. On each delegation the tool:
   - mints the delegation id (as today),
   - sends headers `X-Nav-Repo-Root: <gitRoot>` and `X-Nav-Subpath: <subpath>` on the loopback POST,
   - returns `worktree: agentWorktreePath(agent, id, gitRoot)` so Nav can `git -C <worktree> diff`.

3. **Delegate route bridge.** Replace each delegate's pass-through `route` with a shared handler:
   read `X-Nav-Repo-Root` / `X-Nav-Subpath` + `c.req.param("id")`, **validate** (below), store
   `delegationCtx.set(id, { gitRoot, subpath })`, `await next()`, then `finally` delete the entry.

4. **Delegate factory.** Replace `createAgentWorktree(agent, ctx.id)` with:
   ```ts
   const g = requireDelegationCtx(ctx.id);            // THROWS on miss — never getWorkspaceRoot()
   const cwd = path.join(createAgentWorktree(agent, ctx.id, g.gitRoot), g.subpath);
   ```

5. **`worktrees.ts` generalization.**
   - `createAgentWorktree(agent, id, repoRoot)` and `agentWorktreePath(agent, id, repoRoot)` —
     namespace the worktree dir by `hash(repoRoot)` so different repos don't collide.
   - `pruneAgentWorktrees()` becomes **repo-aware**: `rm -rf tmpdir/nav-worktrees`, then for each
     distinct git root among **all** `nav_projects` rows (archived included) run
     `git -C <gitRoot> worktree prune`. Orphaned metadata for fully-deleted project rows leaks
     harmlessly and is cleared by git on the next worktree op / re-add. `log()` nothing; it's boot.

6. **DRY.** Extract `shared/delegate-runtime.ts`: `requireDelegationCtx(id)`, the route handler
   factory, and `resolveDelegateCwd(agent, id)`. All three delegate files get the identical 2-line
   change (import the shared route + factory body).

### Decisions on the open questions

- **Q1 — header+route bridge is right.** Rejected alternatives: (a) consult creates the worktree
  itself and passes its path — still needs the same map to reach the factory, but the *producer*
  becomes a sibling tool mutating shared state instead of the request's own middleware (worse
  coupling); (b) drop loopback HTTP for in-process `dispatch`/`invoke` with a per-call sandbox —
  abandons the established top-level-agent-with-own-persistence architecture for a much bigger
  redesign. The header travels *with* the request (explicit, debuggable) and the route is the
  natural per-request seam co-located in the delegate module. Minimal and correct.
- **Q2 — no-HEAD repos: don't snapshot, omit the fleet.** Gate on `hasHead`. A freshly `git init`'d
  project runs Nav solo with a clear one-line reason until its first commit. This sidesteps every
  empty-tree snapshot edge case; supporting an empty-tree base is a cheap later add if anyone asks.
- **Q3 — delete in `finally` is safe; add a size cap as a backstop.** `?wait=result` makes
  `routeAgentRequest` await the run, so `next()` resolves only after the agent finishes — the factory
  already consumed the entry. Delegation ids are unique per call, so an id is read once. Keep a
  bounded map (evict oldest beyond N, e.g. 256) purely to bound memory if a future non-wait path ever
  skips the `finally`. No TTL machinery.
- **Q4 — validation is sufficient and injection is covered.** Loopback-only, our own value, but still
  fail-closed: require an absolute path, `existsSync` + `isDirectory`, and
  `git -C <root> rev-parse --show-toplevel` === canonical(root). Newline injection can't reach the
  server because undici/fetch rejects header values containing CR/LF — *and* we re-validate via the
  rev-parse equality check (noting this explicitly so it isn't waved off: the structured single-line
  field is guarded both by the transport and by re-derivation, not by trust).
- **Q5 — prune over all project rows (incl. archived).** That covers active and archived repos; the
  only leak is metadata inside a repo whose project row was fully deleted, which is small and
  self-heals on the next `git worktree` op or re-add. Good enough; don't build a worktree registry.
- **Q7 — risks / v1 assumptions that could break M2:**
  - v1's factory must keep `projectRoot` reachable (M2 needs it to compute `gitCtx`). If v1 only
    computes `cwd` internally, expose `resolveSessionProject` so M2 can reuse it.
  - v1 must keep the consult tools *parameterizable* (M2 turns them into `makeConsult(gitCtx)`); if v1
    hardcodes the singletons it's a small refactor, not a blocker.
  - Delegates: v1 leaves their `route` as pass-through and doesn't touch `worktrees.ts`, so M2 owns
    those files cleanly — no merge conflict with in-flight v1 work.
  - The nav repo always has HEAD, so the default project's fleet behavior is unchanged.
  - Genericized instruction must say the delegate works in **the current project's** checkout and Nav
    writes the final edit in **the active project** checkout (not the nav repo).

### M2 acceptance criteria (must pass before M2 lands)

- **Canonical realpath + subpath containment is enforced, not just checked** (Codex-promoted). The
  delegate route, before stashing, must: canonicalize `gitRoot` via `realpath`; recompute
  `subpath = relative(realpath(gitRoot), realpath(projectRoot))`; and **reject** (fail-closed, no
  worktree) if `subpath` starts with `..` or is absolute — i.e. the project must canonically live
  *inside* its git root after symlink resolution. This is the guard against a symlinked/`..` project
  path escaping the snapshot. Encode it as a test, not a comment.
- A delegation with a missing/invalid `X-Nav-Repo-Root` header **throws in the factory** and never
  snapshots `getWorkspaceRoot()` (fail-closed verified by test).
- A 3-way `consult_panel` in a non-default git project produces three isolated worktrees under
  `hash(gitRoot)`, each cwd'd at the correct `subpath`; `git -C <worktree> diff` returns that
  delegate's changes only.
- A non-git or no-HEAD project exposes **no** consult tools and states the one-line reason.

---

## M3 — Resilience & lifecycle polish

- **Relocate flow (build).** A project whose path is missing on disk (v1 already shows it as
  unavailable) gets a "Locate…" action → folder picker → `PATCH /api/projects/:id { path }`
  (re-canonicalize realpath, re-dedupe, clear the unavailable flag). Repos move; this is real value.
- **Archived-projects browser (cut for now).** Re-adding the path already un-archives (v1). A
  dedicated "Show archived" view is low value; defer until requested.

## M4 — Per-project configuration (mostly cut)

YAGNI for v-next. The agent already reads a project's own `AGENTS.md` through its tools. If a concrete
need appears, the highest-value knobs are: a per-project model override and a per-project
"Nav may edit files without asking" default. Leave a thin seam (the factory already resolves
per-project state, so adding columns later is cheap) but **build nothing now**.

## M5 — Sidebar cosmetics (cut)

Drag-reorder, colors/icons. Lowest value; skip until asked.

## M6 — LLM-generated session titles (small, high daily value)

Deferred from the sessions plan. After the first user+assistant exchange, if
`title_source ∈ {first-message, imported}` (never override `manual`), generate a ≤6-word title and set
`title_source = 'llm'`.
- **Model:** `deepseek/deepseek-v4-flash` — already registered, cheapest, fine for a one-shot
  summarize. (Avoid spending gpt-5.5 on titles.)
- **Trigger:** server-side, lazily — when a session's transcript first gains an assistant message
  (detectable in the same place the list derives freshness), or via a tiny
  `POST /api/sessions/:id/title:generate` the renderer fires once after the first response. Prefer the
  endpoint (explicit, no polling); make it idempotent and a no-op when title is `manual`.
- Independent of M2; can ship anytime.

---

## Recommended order + cut line

1. **M2 — project-aware fleet** (the architectural debt; do first).
2. **M6 — LLM titles** (cheap, daily-visible).
3. **M3a — relocate flow** (resilience).

**Cut below this line until asked:** M3b archived-browser, M4 per-project config, M5 cosmetics.

**Smallest correct cut for this round:** M2 alone is the must-do — it's the only item that's genuine
architecture rather than polish. M6 is a cheap win to pair with it.

## Risks to verify during implementation

- Confirm the runtime invokes the agent factory exactly once per request inside `next()` (so the
  route→map→factory ordering holds); spot-check with a delegation that logs from both route and
  factory.
- Confirm undici/fetch in this Node version rejects CRLF in header values (it does on modern Node) —
  the rev-parse equality check is the real guard regardless.
- Concurrent `git worktree add` on one repo from a `consult_panel` fan-out: git locks
  `.git/worktrees`, so adds serialize; verify no "already locked" flake under 3-way panels.
- `path.relative` edge cases when `projectRoot === gitRoot` (subpath `""` → `path.join(worktree, "")`
  is the worktree root; fine) and when projectRoot is a symlink (canonicalize before relative).
