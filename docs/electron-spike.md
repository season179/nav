# Electron Chat Spike

This is the smallest Electron frontend for nav: a minimal multi-turn chat
window backed by the local HTTP/SSE backend. Electron Main starts the backend,
creates a session, subscribes to its event stream, and relays the user's
messages; the renderer only renders the transcript and submits text through a
narrow preload API.

## Run

Install the Electron dev dependency:

```sh
bun install
```

### With the deterministic mock (no model config needed)

```sh
NAV_MOCK_MODEL=1 bun run electron:dev
```

The mock echoes your latest message and recalls an earlier turn, so multi-turn
context is visible without any API key.

### With a real model (acceptance path)

Export an OpenAI-compatible configuration, then launch:

```sh
export NAV_API_KEY=sk-...           # required
export NAV_MODEL=gpt-4o-mini        # optional, this is the default
export NAV_BASE_URL=https://api.openai.com/v1   # optional, this is the default
bun run electron:dev
```

If neither `NAV_MOCK_MODEL` nor `NAV_API_KEY` is set, the app still runs but a
sent message comes back as a visible "model not configured" error.

### Smoke path

```sh
bun run electron:smoke
```

Launches Electron with the mock model, sends one message, prints
`nav electron smoke received run.completed`, and exits.

### Startup trace

Electron startup tracing is off by default. To collect a local trace, enable it
in `~/.nav/settings.json`:

```json
{
  "observability": {
    "startupTrace": true
  }
}
```

When enabled, Electron writes a small JSONL trace to
`~/.nav/traces/startup.jsonl`. The trace is intentionally sanitized: it records
phase timings, process ids, status, session ids, and model kind, but not prompts,
message text, API keys, raw environment, or provider payloads. The file rotates
after 1 MB, keeping five prior files. Smoke mode also prints a compact startup
summary with total, backend, session-open, and renderer timings.

## Manual Verification

1. Launch with a real model: `NAV_API_KEY=... bun run electron:dev`.
2. Type an initial message (e.g. "My name is Ada.") and submit.
3. Receive an assistant response.
4. Send a follow-up that depends on the prior turn (e.g. "What is my name?").
5. The response shows prior context was included.

## Boundary

- Main starts `cargo run --quiet --bin nav-local-backend -- --bind 127.0.0.1:0`,
  inheriting the environment (so a real `NAV_API_KEY` flows through) and forcing
  `NAV_MOCK_MODEL=1` only in smoke mode.
- Main reads the backend URL from stdout, creates a session over `POST /rpc`,
  and subscribes to `/sessions/{id}/events` over HTTP/SSE.
- Preload exposes only `window.nav.onBackendStatus`, `window.nav.onSessionEvent`,
  and `window.nav.sessionSendMessage(text)`. The send method validates the text
  (must be a non-empty string) before invoking Main, so the renderer can never
  pass an arbitrary IPC payload through.
- The renderer renders the transcript and submits text. It has no access to
  Node, Electron internals, the filesystem, the shell, the backend process, or
  raw `ipcRenderer`.

Renderer isolation is enabled with `contextIsolation: true`,
`nodeIntegration: false`, and `sandbox: true`.
