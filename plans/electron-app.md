# Plan: Electron App as a Future Frontend

Status: planning note. Do not implement this whole document in one shot.

`nav` is first a learning project for building a coding agent and agent
harness. A desktop app can become useful later, but it should not pull the
project away from the backend, protocol, and TUI learning path.

## Recommendation

Do not build the full Electron app as one milestone.

The first Electron milestone should be a small viability slice:

1. Decide whether Electron should attach to the existing local HTTP/SSE
   protocol or introduce a separate child-process transport.
2. Start a minimal Electron shell with a secure renderer/preload/main boundary.
3. Launch or attach to the local Rust backend from Electron Main.
4. Render one existing session event stream read-only.
5. Add one command path, probably `session.sendMessage`.

Approvals, durable replay, crash recovery, multi-window behavior, packaging,
auto-update, and polished desktop UI should come later.

## Why Not One Shot

The original shape combines too many separate projects:

- Electron shell and window lifecycle.
- Renderer security, preload API design, and IPC filtering.
- Backend launch, supervision, restart, and logs.
- Protocol transport choice.
- Session stream rendering.
- Command acknowledgement and cancellation.
- Approvals.
- Durable session/event replay.
- Crash recovery.
- Packaging and distribution.

That is too much surface area for one implementation pass. It also risks
adding dormant desktop code that makes `nav` harder to understand before the
backend and protocol are settled.

## Transport Decision

Prefer the existing `nav` protocol first:

```text
commands: POST /rpc
events:   GET /sessions/{session_id}/events
```

The current architecture docs describe JSON-RPC over HTTP for commands and SSE
for ordered session events. Electron should start by acting as another frontend
client of that contract, not by inventing a second protocol.

Recommended first shape:

```text
Electron Renderer
  -> Electron Preload API
  -> Electron Main Process
  -> local HTTP/SSE client
  -> nav-server
```

Use a `stdin/stdout` transport only if there is a concrete reason the HTTP/SSE
path cannot serve a packaged desktop app. If that path is chosen later, treat it
as a deliberate protocol adapter, not as the primary protocol.

Important Electron note: Electron `utilityProcess` does not support writable
`stdin`. A `stdin/stdout` protocol would usually mean Node `child_process.spawn`
from Electron Main, with all the supervision and packaging details that implies.

## Security Boundary

Electron Main is the only process that may supervise or talk directly to the
Rust backend.

The renderer should never receive raw Node, Electron, filesystem, shell, or
backend process access. Keep `nodeIntegration` off, keep `contextIsolation` on,
and expose only a narrow typed API through preload.

Good preload shape:

```ts
window.nav.sessionSendMessage({ sessionId, text });
window.nav.onSessionEvent(sessionId, callback);
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

- Render sessions, assistant output, tool calls, diffs, and approvals.
- Hold local presentation state.
- Call only the safe preload API.
- Avoid any direct process, filesystem, shell, or backend transport access.

### Electron Preload

- Expose a minimal typed frontend API.
- Validate renderer calls before invoking Electron Main IPC.
- Subscribe renderer code to typed backend events.
- Hide `ipcRenderer` itself.

### Electron Main

- Create and manage windows.
- Start, locate, or attach to the Rust backend.
- Own backend supervision, restart, and shutdown behavior.
- Speak the backend protocol.
- Route backend responses/events to renderer windows.
- Enforce IPC method allowlists.

### Rust Backend

- Own sessions, runs, messages, tool calls, approvals, and event history.
- Execute agent runs and tool operations.
- Own filesystem, Git, shell, model, and policy side effects.
- Persist canonical session state.
- Expose protocol projections for frontends.

## Protocol Model

Commands use request/response JSON-RPC semantics. The response acknowledges that
the command was accepted and returns correlation IDs.

Example command:

```json
{
  "jsonrpc": "2.0",
  "id": "019f2f6f-f178-7a72-9f28-000000000001",
  "method": "session.sendMessage",
  "params": {
    "sessionId": "019f2f6f-f178-7a72-9f28-000000000100",
    "text": "Explain this repo"
  }
}
```

Long-running output is delivered through session events, not by holding the
command response open until the run finishes.

Core identifiers:

| Identifier | Purpose |
| --- | --- |
| `id` | JSON-RPC request/response correlation |
| `sessionId` | Long-lived coding-agent session |
| `runId` | One execution or agent turn |
| `eventId` | Ordered session event identity |
| `toolCallId` | Tool start/completion correlation |
| `approvalId` | Approval request/response correlation |

## Persistence Model

Do not make the Electron app the source of truth for session history.

The durable source of truth belongs in the Rust backend. Electron should rebuild
its UI from backend state and event replay.

The backend should own:

- sessions
- runs
- turns and turn parts
- events
- approvals
- tool calls
- file changes

The desktop app may keep small local UI preferences, but not canonical
transcripts or provider state.

## Phased Milestones

### Phase 0: Transport Decision

- Re-read the current protocol docs and implementation.
- Confirm whether Electron can use local HTTP/SSE unchanged.
- Document any reason to add a `stdin/stdout` adapter.
- Do not create desktop UI scaffolding yet.

Exit criteria:

- One written decision: HTTP/SSE first, or a justified adapter.

### Phase 1: Read-Only Desktop Spike

- Add the smallest Electron shell.
- Keep the renderer secure by default.
- Start or attach to the backend from Electron Main.
- Subscribe to one session event stream.
- Render streamed messages read-only.

Exit criteria:

- A local desktop window can display an existing session stream.
- No command execution, approvals, filesystem access, or packaging work.

### Phase 2: One Command Path

- Add `session.sendMessage`.
- Route command acknowledgement back to the renderer.
- Continue rendering run events from the event stream.
- Handle basic backend unavailable and reconnect states.

Exit criteria:

- A user can send one prompt and see the response stream.

### Phase 3: Approvals and Tool Visibility

- Render tool calls and approval requests.
- Add typed approve/reject preload methods.
- Keep approval decisions in the backend protocol.

Exit criteria:

- A desktop user can approve or reject a pending tool request.

### Phase 4: Durable Resume

- Rebuild desktop UI state from backend session storage.
- Support event replay and reconnect behavior.
- Handle backend restart without losing canonical session history.

Exit criteria:

- Closing and reopening the desktop app restores the session from backend state.

### Phase 5: Desktop Product Work

- Multi-window behavior.
- App menus and keyboard shortcuts.
- File/diff viewing.
- Notifications.
- Packaging, signing, and updates.

Exit criteria:

- The app is useful as a desktop product, not only as a protocol spike.

## Key Design Rules

- Electron is a frontend, not a second backend.
- Use the existing protocol before creating a new transport.
- Keep Electron-specific concepts out of the Rust harness.
- Keep backend-owned side effects behind Rust protocol methods.
- Keep the renderer isolated from Node, Electron internals, and process access.
- Do not stream multiple JSON-RPC responses with the same request ID.
- Use event IDs and run/session IDs for long-running output correlation.
