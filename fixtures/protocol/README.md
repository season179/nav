# nav protocol fixtures

These fixtures are shared examples for clients that speak nav's frontend/backend
protocol. They cover JSON-RPC command envelopes and Server-Sent Event streams
without depending on the Go TUI implementation.

The fixtures are protocol projections, not the durable transcript schema. The
future source of truth is still the session store described in
`plans/session-storage.md`: sessions have runs, runs produce ordered turns, and
turns are made of parts. These files show what frontends send and receive at the
wire boundary.

For example, `session.sendMessage` acknowledges a protocol `runId` and
`messageId`, then SSE projects run and message events for display. A future
storage implementation may persist those as session rows, run rows, ordered
turns, and text/tool parts, but clients should not treat these fixtures as
SQLite rows or provider-native response history.

## Layout

- `json-rpc/` contains representative JSON-RPC 2.0 requests and responses.
- `event-streams/` contains SSE frames with `id`, `event`, and JSON `data`.
- `provider-streams/` contains provider-native Chat Completions SSE frames used
  by fake upstream servers in backend regression tests.

## Using the fixtures

A non-TUI frontend can load the request fixtures as JSON-RPC examples, send
matching commands to its backend client, and compare response envelopes by
method, result keys, error shape, and ID relationships. Generated IDs do not
need to match these literal examples.

For SSE, parse each frame into `id`, `event`, and `data`. Check that `id`
matches `data.event_id`, `event` matches `data.type`, events arrive in the same
order, and reconnect with `Last-Event-ID` resumes after the referenced event.

All IDs are valid illustrative lowercase UUIDv7 strings. A running server will
generate its own IDs, so conformance tests should compare the envelope shape,
event order, method names, and cross-field relationships instead of requiring
byte-for-byte ID equality.

## Client identity

`json-rpc/initialize-request.json` demonstrates the intended client identity and
capability negotiation surface for future non-TUI frontends. The server routes
`initialize` and returns the currently supported methods, event types, and
transport capabilities.

## Replay

`event-streams/replay-after-run-started.sse` represents reconnecting to
`GET /sessions/{sessionId}/events` with `Last-Event-ID` set to the
`run.started` event ID from `event-streams/message-send-completed.sse`. The
server should resume strictly after that event.

## Provider stream fixtures

`provider-streams/delayed-chat-completions.sse` is upstream provider output, not
nav frontend protocol output. Tests should flush the first `data:` frame, pause
before the `: delayed boundary` marker, then flush the remaining frames. That
shape proves the backend publishes partial model text before the provider stream
finishes and keeps replay cursors valid while a run is still active.
