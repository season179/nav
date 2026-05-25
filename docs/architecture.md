# Architecture

Status: accepted direction. The current prototype may lag behind this document.

## Intent

`nav` is a learning project: a coding agent built from the ground up to make
the agent loop, tool execution, permissions, context handling, and terminal UI
easier to understand by building them directly.

The primary purpose is learning by building a coding agent. The secondary
purpose is to grow it into a personalized coding agent for Season's own
workflow.

That purpose should shape architecture decisions. Prefer clear boundaries,
plain names, and easy-to-follow code over clever abstractions. The project
should be understandable to someone studying how coding agents work.

## Product Shape

`nav` is a Rust coding agent backend with replaceable frontends.

The first frontend is the Go Bubble Tea TUI, but the backend protocol must also
support future Electron and web frontends.

## Repository Folders

- `crates/` contains Rust crates.
- `crates/nav-backend/` contains the coding agent backend.
- `docs/` contains architecture and design notes.
- `tui/` contains the Go Bubble Tea terminal frontend.
- `tui/cmd/` contains executable entrypoints only.
- `tui/cmd/nav/` is the released TUI command.
- `tui/cmd/navd/` is the local development entrypoint.
- `tui/internal/` contains private Go packages for the TUI module.
- `tui/internal/app/` wires the backend client into the Bubble Tea program.
- `tui/internal/client/` owns the frontend-to-backend API client.
- `tui/internal/localdev/` owns local checkout build/run behavior for `navd`.
- `tui/internal/ui/` owns Bubble Tea state, updates, rendering, layout, and
  styles.

Generated folders such as `target/` and `.cache/` are build artifacts, not
source architecture.

## Boundaries

The Rust backend owns agent state and side effects:

- sessions, runs, messages, tool calls, approvals, and event history
- model/provider orchestration
- filesystem and shell access
- permissions and policy decisions

Frontends own presentation:

- layout, rendering, input handling, and local UI preferences
- reconnect and resume UX
- temporary optimistic UI state

The protocol should not expose Bubble Tea, Electron, or browser-specific
concepts.

## TUI Package Rules

The Go TUI should keep entrypoints thin and package names boring:

- `cmd/*` should delegate immediately to internal packages.
- `internal/app` is the composition point, not a domain layer.
- `internal/client` is the backend protocol boundary.
- `internal/ui` should translate backend events into UI state.
- Rendering, styles, layout helpers, and input handling should stay split once
  they become large enough to read independently.

`navd` should not become a product subsystem. It is a development convenience
for running local builds without confusing them with released `nav`.

## Protocol

Frontend commands use JSON-RPC 2.0 over HTTP:

```text
POST /rpc
```

Initial methods:

- `initialize`
- `session.create`
- `session.sendMessage`
- `run.cancel`
- `tool.approve`
- `tool.reject`
- `session.close`

Command responses acknowledge state changes. Long-running agent output is
delivered through events, not by holding a command response open until the run
finishes.

Backend events use Server-Sent Events:

```text
GET /sessions/{session_id}/events
```

Initial event types:

- `session.created`
- `run.started`
- `message.delta`
- `message.completed`
- `tool.call_requested`
- `tool.call_started`
- `tool.call_completed`
- `tool.approval_requested`
- `file.changed`
- `run.completed`
- `run.cancelled`
- `run.failed`
- `error`

Example event:

```text
id: 019f2f70-8a7c-7c4d-9d2f-4d9dce42f229
event: message.delta
data: {"event_id":"019f2f70-8a7c-7c4d-9d2f-4d9dce42f229","session_id":"019f2f6f-f178-7a72-9f28-7f9aa0a1c853","run_id":"019f2f70-1ec4-7f13-8658-feb64178952d","message_id":"019f2f70-6cc4-79c9-98fb-bd7c3f2419d8","text":"hello"}
```

Clients should reconnect with `Last-Event-ID`. The backend should keep enough
event history to resume an interrupted frontend.

## Transport

The default backend transport is local HTTP:

- bind to `127.0.0.1` by default
- choose a random available port unless explicitly configured
- require a random local auth token or secure local cookie
- deny broad CORS by default

Frontend launchers may spawn the backend and discover its URL/token through a
small bootstrap mechanism. Stdio may be used for bootstrap or logs, but not as
the application protocol.

## IDs

All protocol-visible IDs are canonical lowercase UUIDv7 strings.

This includes:

- JSON-RPC request `id`
- `session_id`
- `run_id`
- `message_id`
- `tool_call_id`
- `approval_id`
- `event_id`
- `file_change_id`

IDs are not secrets. UUIDv7 leaks approximate creation time, so authentication
must use separate random tokens.

If storage needs a private sequence or cursor for exact event replay ordering,
keep it internal and do not expose it as a protocol resource ID.

## Current Gap

The prototype currently starts the backend over stdio and only performs a hello
check from the TUI. The next backend milestone is to replace that path with the
HTTP JSON-RPC plus SSE architecture described here.
