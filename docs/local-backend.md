# Local HTTP/SSE Backend

This is the smallest runnable backend surface for the nav chat slice: a
multi-turn coding-agent loop backed by one text model. It owns sessions, message
history, context assembly, the model call, tool execution, durable session
storage, and the session event stream. It has no approval flow yet, so the
backend must stay bound to loopback.

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

The backend resolves one text model. The preferred source is the Pi-style
`~/.nav/settings.json` default model (issue #531); a few environment variables
remain as fallbacks.

| Variable | Purpose | Default |
| --- | --- | --- |
| `NAV_MOCK_MODEL` | If set (any non-empty value), use the deterministic mock model. Wins over everything else. | unset |
| `NAV_API_KEY` | API key for an OpenAI-compatible provider. Used only when no settings file exists. | unset |
| `NAV_MODEL` | Model name for the env fallback. | `gpt-4o-mini` |
| `NAV_BASE_URL` | Base URL for the env fallback. | `https://api.openai.com/v1` |

Resolution order:

1. `NAV_MOCK_MODEL` set → deterministic mock (used by tests and offline smoke).
2. otherwise `~/.nav/settings.json` resolves a default model → real
   OpenAI-compatible model built from the resolved `apiKey`, `model`, and
   `baseUrl`. Only `api: "openai-completions"` is supported.
3. otherwise, if **no settings file exists**, fall back to `NAV_API_KEY` → real
   OpenAI-compatible model.
4. otherwise → not configured. Sending a message emits a `run.failed` event
   with a clear "model not configured" message instead of guessing or
   hardcoding any secret.

A settings file that exists but cannot be used (unsupported API, missing
provider/model, malformed JSON, unresolvable key) does **not** silently fall
back: the backend stays up and reports the specific reason on the first
`session.sendMessage` as a `run.failed` event.

Only the resolved API key — never logged, never sent to the renderer, and
redacted in debug output — is used as the provider `Authorization` header.

Token counts are operational estimates for future context management, not
billing data. When an OpenAI-compatible response includes `usage`, nav records
that. Otherwise it estimates locally. A model `compat` block may opt into a
Hugging Face tokenizer with either `tokenizerPath` or
`tokenizer: { "path": "...", "id": "..." }`; when no tokenizer is configured or
loadable, nav uses a conservative heuristic.

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
next so the model sees consistent Model Context assembled from ordered Turn
History.

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
- `assistant.tool_calls`
- `tool.started`
- `tool.completed`
- `tool.failed`
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

## Manual Verification (real model via `~/.nav/settings.json`)

This is the end-to-end path through the Electron app and the configured default
model (e.g. `provider: commandcode`, `model: Qwen/Qwen3.7-Max`,
`api: openai-completions`).

1. Confirm the settings file resolves a default model (issue #531). With no
   `NAV_MOCK_MODEL`/`NAV_API_KEY` set, the backend prints the resolved model to
   stderr on startup:

   ```sh
   cargo run --bin nav-local-backend -- --bind 127.0.0.1:8788
   # stderr: nav-local-backend: using OpenAI-compatible model Qwen/Qwen3.7-Max
   ```

   If it instead prints `model unavailable: ...`, the file exists but is not
   usable — the message names the reason (unsupported API, missing provider,
   etc.).

2. Launch the Electron app (it spawns the backend itself and inherits your
   environment, so leave `NAV_MOCK_MODEL` unset to reach the real model):

   ```sh
   npm run electron:dev
   ```

3. Send an initial message (e.g. `my name is Ada`) and confirm a **real**
   assistant response renders.
4. Send a follow-up that depends on it (e.g. `what is my name?`) and confirm the
   reply reflects the earlier turn — proving prior conversation context was
   forwarded to the provider.

If the provider request fails or returns an unexpected shape, the app renders a
`run.failed` event with the reason and stays usable; no API key or auth header
appears in logs, errors, or the renderer.
