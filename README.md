# nav

`nav` is a learning project: a coding agent built from the ground up to make
the agent loop, tool execution, permissions, context handling, and desktop UI
easier to understand by building them directly.

The primary purpose is learning by building a coding agent. The secondary
purpose is to grow it into a personalized coding agent for my own workflow.

Do not depend on `nav` yet. The project is early, experimental, and likely to
change drastically as the architecture becomes clearer.

## Architecture

nav is now a two-package pnpm workspace:

- `backend/` is a Flue Node backend. It defines the `nav` coding agent, owns
  the local HTTP control plane, persists Flue conversation state, and stores
  nav-specific session/catalog data.
- `desktop/electron/` is the Electron shell and React renderer. Electron Main
  spawns the local Flue backend and exposes OS-only capabilities through the
  preload bridge. The renderer uses TanStack Query, Router, Store, Virtual,
  Form, Table, Pacer, and dev-only Devtools for the app experience.

The backend prints the readiness line Electron watches for:

```text
nav local backend listening on http://127.0.0.1:<port>
```

## Development

Use Node 24 and pnpm 11, matching `.nvmrc` and `package.json` engines.

```sh
pnpm install
pnpm run typecheck
pnpm run lint
pnpm run check:electron
```

Run the Electron app:

```sh
pnpm run electron:dev
```

Run only the backend:

```sh
pnpm --dir backend run build
pnpm --dir backend run start
```

The backend starts keyless for health, OpenAPI, catalog, and UI plumbing checks.
Real model turns need the provider key in the process environment, for example
`ANTHROPIC_API_KEY` or `OPENAI_API_KEY`, before starting the backend or Electron.

## Local Data

Runtime state is ignored under `backend/data/`:

- `flue.db` is the durable source of truth for Flue conversation history:
  sessions, entries, submissions, and event streams.
- `sessions.json` is nav's durable source of truth for discoverability and
  per-session configuration: sidebar summaries, model, thinking level, mode,
  workspace, and worktree metadata.
- `stacks.json` is a disposable observability sidecar for sanitized model-turn
  stack captures. It must not be required to resume or render a chat.
- `worktrees/` contains per-session git worktrees for worktree mode.

All three stores are keyed by the same `sessionId`, but they are not written in
one transaction. Code that reads them must tolerate drift: a catalog entry may
exist before a Flue thread has any events, stack rows may be missing or pruned,
and stale sidecar rows should be harmless. Reset removes all local backend
state; delete flows should remove the catalog entry, stack sidecar rows, and any
generated worktree for that session.

Reset local backend state with:

```sh
pnpm run reset-data
```

## License

MIT. See `LICENSE`.
