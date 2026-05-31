# Local HTTP/SSE Backend

This is the smallest runnable backend surface for the nav chat slice: an
in-memory, multi-turn chat loop backed by one text model. It owns sessions,
message history, the model call, and the session event stream. It has no tools,
no file or shell access, no approvals, and no durable persistence.

## Start

```sh
cargo run --bin nav-local-backend -- --bind 127.0.0.1:0
```

The backend prints its discoverable URL as the first stdout line:

```text
nav local backend listening on http://127.0.0.1:54321
```

It also prints the resolved model to stderr, e.g. `nav-local-backend: using
mock model`.

## Model Configuration

The backend resolves one text model from the environment:

| Variable | Purpose | Default |
| --- | --- | --- |
| `NAV_MOCK_MODEL` | If set (any non-empty value), use the deterministic mock model. Wins over `NAV_API_KEY`. | unset |
| `NAV_API_KEY` | API key for an OpenAI-compatible provider. Selects the real model. | unset |
| `NAV_MODEL` | Model name for the real provider. | `gpt-4o-mini` |
| `NAV_BASE_URL` | Base URL for the OpenAI-compatible API. | `https://api.openai.com/v1` |

Resolution order:

1. `NAV_MOCK_MODEL` set → deterministic mock (used by tests and offline smoke).
2. otherwise `NAV_API_KEY` set → real OpenAI-compatible model.
3. otherwise → not configured. Sending a message emits a `run.failed` event
   with a clear "model not configured" message instead of guessing or
   hardcoding any secret.

No API keys are read from anywhere except the environment, and none are logged.

## Command Channel: `POST /rpc`

JSON-RPC 2.0 over HTTP. Two methods back the chat loop.

Create a session:

```sh
curl -s -X POST http://127.0.0.1:54321/rpc \
  -d '{"jsonrpc":"2.0","id":"1","method":"session.create"}'
# => {"jsonrpc":"2.0","id":"1","result":{"sessionId":"019..."}}
```

Send a message (the model call runs in the background; progress arrives over the
event stream):

```sh
curl -s -X POST http://127.0.0.1:54321/rpc \
  -d '{"jsonrpc":"2.0","id":"2","method":"session.sendMessage",
       "params":{"sessionId":"019...","text":"Hello"}}'
# => {"jsonrpc":"2.0","id":"2","result":{"accepted":true}}
```

Send one message per turn: wait for a turn's `run.completed` before sending the
next so the model sees a consistent, ordered history.

## Session Event Stream: `GET /sessions/{id}/events`

A live Server-Sent Events feed. On connect it replays the session's current
event backlog, then streams new events as they happen.

```sh
curl -N http://127.0.0.1:54321/sessions/019.../events
```

Each frame carries an `id`, an `event`, and a flat JSON `data` envelope:

```text
id: 019f...
event: message.completed
data: {"event_id":"019f...","session_id":"019...","type":"message.completed","sequence":3,"run_id":"019...","message_id":"019...","role":"assistant","text":"..."}
```

The event types are:

- `session.created`
- `user.message`
- `run.started`
- `message.completed`
- `run.completed`
- `run.failed`

## Manual Verification (mock, no real model needed)

```sh
NAV_MOCK_MODEL=1 cargo run --bin nav-local-backend -- --bind 127.0.0.1:8787 &
SID=$(curl -s -X POST http://127.0.0.1:8787/rpc \
  -d '{"jsonrpc":"2.0","id":"1","method":"session.create"}' \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"]["sessionId"])')
curl -N http://127.0.0.1:8787/sessions/$SID/events &
curl -s -X POST http://127.0.0.1:8787/rpc \
  -d "{\"jsonrpc\":\"2.0\",\"id\":\"2\",\"method\":\"session.sendMessage\",
       \"params\":{\"sessionId\":\"$SID\",\"text\":\"my name is Ada\"}}"
# Then send a follow-up like "what is my name?" with the same sessionId; the
# mock reply recalls the earlier turn, proving multi-turn context.
```
