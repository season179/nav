# Electron Read-Only Spike

This is the smallest Electron frontend slice for `nav`. It launches the local
backend fixture from Electron Main, subscribes to one HTTP/SSE session stream,
and renders the received events read-only.

## Run

Install the Electron dev dependency:

```sh
bun install
```

Open the desktop spike:

```sh
bun run electron:dev
```

Run the smoke path that opens Electron, receives the deterministic
`run.completed` event, then exits:

```sh
bun run electron:smoke
```

## Boundary

- Main starts `cargo run --quiet --bin nav-local-backend -- --bind 127.0.0.1:0`.
- Main reads the backend URL from stdout.
- Main subscribes to
  `/sessions/019f2f6f-f178-7a72-9f28-000000000100/events` over HTTP/SSE.
- Preload exposes only `window.nav.onBackendStatus` and
  `window.nav.onSessionEvent`.
- Renderer displays status and events; it does not send prompts, approvals,
  filesystem requests, shell requests, or raw IPC messages.

Renderer isolation is enabled with `contextIsolation: true`,
`nodeIntegration: false`, and `sandbox: true`.
