# Local HTTP/SSE Backend

This is the smallest runnable backend surface for frontend spikes. It is a
read-only fixture, not the full agent loop.

## Start

```sh
cargo run --bin nav-local-backend -- --bind 127.0.0.1:0
```

The backend prints its discoverable URL as the first stdout line:

```text
nav local backend listening on http://127.0.0.1:54321
```

Use that URL for local clients. An Electron main process can either:

- start the backend with `--bind 127.0.0.1:0` and read this first stdout line;
- choose a concrete loopback address itself, pass it through `--bind`, and hand
  the resulting URL to the renderer through preload IPC.

## Fixture Session Stream

The deterministic fixture session ID is:

```text
019f2f6f-f178-7a72-9f28-000000000100
```

Subscribe to the stream:

```sh
curl -N http://127.0.0.1:54321/sessions/019f2f6f-f178-7a72-9f28-000000000100/events
```

Each SSE frame has an `id`, an `event`, and a JSON `data` envelope:

```text
id: 019f2f6f-f178-7a72-9f28-000000000101
event: session.created
data: {"event_id":"019f2f6f-f178-7a72-9f28-000000000101","session_id":"019f2f6f-f178-7a72-9f28-000000000100","type":"session.created","sequence":0}
```

The fixture currently emits:

- `session.created`
- `run.started`
- `message.delta`
- `message.completed`
- `run.completed`

The stream closes after replaying the deterministic fixture events. Durable
session storage, live fan-out, model execution, approvals, and replay trimming
are intentionally not implemented here.

## Command Channel

`POST /rpc` exists only as an explicit placeholder. It returns HTTP `501` with
`{"error":"rpc_deferred", ...}` so frontend spikes can distinguish "command
channel intentionally deferred" from "route missing".
