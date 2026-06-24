# Plan: Electron App Frontend

Status: current orientation note. The implementation plan lives in
`docs/plan-flue-tanstack-rewrite.md`; this document records the product and
boundary decisions for the desktop app.

`nav` is first a learning project for building a coding agent and agent
harness. The desktop app should make that learning surface usable without
turning Electron into a second backend.

## Recommendation

Keep Electron small and focused:

1. Electron Main supervises the local Flue backend and owns OS-only capabilities.
2. The preload bridge exposes a narrow typed API for backend URL discovery,
   directory picking, and the "Start in" preference.
3. The renderer talks to the local HTTP control plane and Flue agent streams as
   an app client.
4. Canonical sessions, transcript history, model configuration, stacks, and
   worktree paths stay backend-owned.

Approvals, durable replay hardening, multi-window behavior, packaging,
auto-update, and polished desktop UI can build on that foundation later.

## Why Not One Shot

The desktop app still spans several separate concerns:

- Electron shell and window lifecycle.
- Renderer security, preload API design, and IPC filtering.
- Backend launch, supervision, shutdown, and logs.
- HTTP control-plane reads and mutations.
- Flue SSE stream rendering.
- Command acknowledgement and cancellation semantics.
- Approvals.
- Durable session and event replay.
- Crash recovery.
- Packaging and distribution.

Keep each slice independently verifiable so the app remains understandable while
the harness evolves.

## Transport Decision

Decision: Electron uses the local Flue backend over HTTP plus Flue's native SSE
streaming routes.

```text
control plane: /nav/*
agent send:    POST /agents/nav/:id
agent stream:  GET /agents/nav/:id?live=sse&offset=...
```

Electron Main still watches for the backend readiness line:

```text
nav local backend listening on http://127.0.0.1:<port>
```

The renderer should receive the resolved base URL through the preload bridge,
then use normal HTTP and SSE clients from the sandboxed renderer process.

Recommended shape:

```text
Electron Renderer
  -> local HTTP/SSE client
  -> Flue backend

Electron Renderer
  -> Electron Preload API
  -> Electron Main Process
  -> OS-only operations
```

Use a process stdio transport only if there is a concrete packaging reason the
HTTP/SSE path cannot serve the desktop app. Treat that as a later adapter, not
as the primary app protocol.

Important Electron note: Electron `utilityProcess` does not support writable
`stdin`. A stdio protocol would usually mean Node `child_process.spawn` from
Electron Main, with all the supervision and packaging details that implies.

## Security Boundary

Electron Main is the only process that may supervise the backend or touch
native desktop capabilities.

The renderer should never receive raw Node, Electron, filesystem, shell, or
backend process access. Keep `nodeIntegration` off, keep `contextIsolation` on,
and expose only a narrow typed API through preload.

Good preload shape:

```ts
window.nav.getBackendUrl();
window.nav.pickDirectory();
window.nav.getStartMode();
window.nav.setStartMode("worktree");
```

Bad preload shape:

```ts
window.electron.ipcRenderer.send(channel, payload);
```

The preload API should validate method names and payloads instead of passing
arbitrary IPC messages through to Electron Main.

Reference docs:

- https://www.electronjs.org/docs/latest/tutorial/security
- https://www.electronjs.org/docs/latest/tutorial/context-isolation
- https://www.electronjs.org/docs/latest/api/utility-process

## Responsibilities

### Electron Renderer

- Render sessions, assistant output, tool calls, stacks, diffs, and approvals.
- Hold local presentation state.
- Read and mutate backend state through TanStack Query.
- Feed Flue stream events into the TanStack Store-backed session reducer.
- Call only the safe preload API for OS-owned work.
- Avoid any direct process, filesystem, shell, or backend supervision access.

### Electron Preload

- Expose a minimal typed frontend API.
- Validate renderer calls before invoking Electron Main IPC.
- Hide `ipcRenderer` itself.

### Electron Main

- Create and manage windows.
- Start and stop the local Flue backend.
- Detect the backend readiness line and expose the resolved base URL.
- Own backend supervision, restart, and shutdown behavior.
- Own OS-only capabilities such as directory picking and local preferences.
- Enforce IPC method allowlists.

### Flue Backend

- Own sessions, runs, transcript persistence, model choices, stacks, and
  worktree metadata.
- Execute agent runs with Flue's local sandbox.
- Own filesystem, Git, shell, model, and policy side effects.
- Expose the `/nav/*` control plane and Flue native agent routes for frontends.

## Product Milestones

### Phase 0: Backend Reachability

- Main starts the Flue backend.
- Renderer receives the backend URL.
- Health and OpenAPI endpoints answer keyless.

Exit criteria:

- A local desktop window can reach the backend without provider keys.

### Phase 1: Session Shell

- Create, list, select, resume, and delete sessions.
- Preserve the chosen workspace and start mode.
- Reflect backend unavailable and reconnect states.

Exit criteria:

- A user can create a session and relaunch into it.

### Phase 2: Live Chat

- Send a prompt through Flue's native agent route.
- Subscribe to the stream with offset handling.
- Render live text, completion, errors, and tool lifecycle messages.

Exit criteria:

- A user can send one prompt and see the response stream.

### Phase 3: Model, Settings, And Stacks

- Switch model and thinking level per session.
- Configure "Start in" mode through the settings view.
- Render captured stack rows with sortable/filterable affordances.

Exit criteria:

- A user can inspect and adjust the execution context for a session.

### Phase 4: Durable Resume

- Rebuild desktop UI state from backend session storage.
- Support stream reconnect and offset resume behavior.
- Handle backend restart without losing canonical session history.

Exit criteria:

- Closing and reopening the desktop app restores the selected session from
  backend state.

### Phase 5: Desktop Product Work

- Approval UX.
- Multi-window behavior.
- App menus and keyboard shortcuts.
- File/diff viewing.
- Notifications.
- Packaging, signing, and updates.

Exit criteria:

- The app is useful as a desktop product, not only as a harness UI.

## Key Design Rules

- Electron is a frontend and supervisor, not a second backend.
- Use the local HTTP/SSE backend before creating a new transport.
- Keep backend-owned side effects behind backend routes.
- Keep the renderer isolated from Node, Electron internals, and process access.
- Use Flue stream offsets and run/session IDs for long-running output
  correlation.
