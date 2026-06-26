# nav Flue Backend

This package is the local Flue backend for the nav Electron app. It serves the
Flue `nav` agent, a nav-owned HTTP control plane, health checks, and a small
OpenAPI document.

## Commands

```sh
pnpm --dir backend run typecheck
pnpm --dir backend run test
pnpm --dir backend run build
pnpm --dir backend run start
```

`pnpm --dir backend run start` expects `dist/server.mjs`, so build first. The
start wrapper reserves a local port, launches the generated Flue server with
`PORT` pinned, and mirrors Flue's startup log with the Electron-compatible line:

```text
nav local backend listening on http://127.0.0.1:<port>
```

`--port <number>` or `PORT=<number>` can pin the port. With `0` or no port, the
wrapper chooses an available local port before starting the generated server.

## HTTP Surface

- `GET /health` returns backend readiness.
- `GET /openapi.json` returns the authored local API summary.
- `/agents/nav/:id` is Flue's native agent route for prompt submission and SSE
  event streaming.
- `/nav/*` is the nav control plane for sessions, models, thinking level,
  stop, stacks, and stack availability.

## State Contract

Flue and nav intentionally store different state. The `sessionId` is the shared
key across every store, but no transaction spans the stores, so callers must
treat cross-store drift as normal and recoverable.

- `src/db.ts` configures Flue SQLite at `data/flue.db`. This is the durable
  source of truth for conversation history: Flue sessions, entries,
  submissions, and event streams. nav should not duplicate transcript state in
  its own catalog.
- `data/sessions.json` is nav's durable source of truth for discoverability and
  per-session configuration because Flue does not enumerate agent instances. It
  stores sidebar summaries, per-session model and thinking level, local/worktree
  mode, workspace paths, and generated worktree paths.
- `data/stacks.json` is a disposable observability sidecar. It stores sanitized
  stack rows captured from Flue observation events and must not be required to
  send, resume, or render a chat.

Recovery rules:

- A catalog entry with no Flue event stream is a valid empty session.
- Missing or pruned stack rows mean stack details are unavailable, not that the
  session is invalid.
- Stack rows without a catalog entry are stale sidecar data and should be
  ignored by normal session flows.
- If a catalog entry points at a missing worktree, worktree-mode operations may
  fail for that session, but local-mode sessions and other sessions should keep
  working.
- The renderer's TanStack Store state is only a live projection of Flue events.
  It is not durable state and may be rebuilt from the backend stream.

Lifecycle expectations:

- Creating a session writes the catalog first; Flue creates conversation state
  when the first prompt is accepted.
- Resuming a session updates catalog recency and reopens the Flue stream for
  the same `sessionId`.
- Deleting a session should remove the catalog entry, remove stack sidecar rows,
  and remove that session's generated worktree when present. Flue conversation
  deletion should be added here if nav begins exposing durable transcript delete.
- Resetting backend data removes the full local runtime state set:
  `flue.db`, `sessions.json`, `stacks.json`, and generated worktrees.

The catalog path can be overridden with `NAV_SESSION_CATALOG_PATH`. Stack
storage can be overridden with `NAV_STACKS_PATH`.

## Agent And Models

`src/agents/nav.ts` defines the `nav` coding agent with Flue's `local()`
sandbox. Each agent interaction looks up `context.id` in the nav catalog so the
session's model, thinking level, and working directory are applied at runtime.

The model catalog is intentionally nav-owned in `src/model-catalog.ts`. It
defaults to Claude Sonnet 4.6 and GPT-5 entries, with the default selected by
`NAV_DEFAULT_MODEL` and `NAV_DEFAULT_THINKING_LEVEL` when provided.

Real model turns read provider keys from the backend process environment, such
as `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`. Electron inherits the user's
environment and passes it through to the backend process.

Offline Electron smoke runs set `NAV_MOCK_MODEL=1`. In that mode the backend
registers a local `nav-mock/nav-smoke` provider, selects it for new sessions,
and streams a deterministic assistant response without reading any provider
key.

## Modes

`local` mode runs the agent in the selected workspace.

`worktree` mode requires the selected workspace to be inside a git repository.
The catalog creates a detached git worktree under `data/worktrees/<sessionId>`
by default, then points the agent `cwd` at that worktree. `NAV_WORKTREE_DIR` can
override the worktree base directory. Deleting a session removes the matching
worktree with `git worktree remove --force`, falling back to filesystem removal
if git cleanup fails.

## Stop And Stacks Behavior

`session.stop` currently returns `{ "stopped": false }`. Flue beta.5 documents
in-process cancellation primitives, but this backend has no verified HTTP
endpoint that guarantees durable submission cancellation.

Stacks are implemented with `observe()` in `src/stacks.ts`. The store records
`turn_request` and `turn` observations, sanitizes request/response payloads for
JSON storage, and serves them through `/nav/sessions/:sessionId/stacks`.
Availability returns `{ "available": true }` only after the session has at
least one retained stack record.
