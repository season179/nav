# Headless JSON-RPC Protocol

This is the stable v1 contract for non-TUI frontends such as `nav-desktop`,
chat adapters, dashboards, and test harnesses.

`nav --json-events` remains the raw debugging stream: one `AgentEvent` JSON
object per stdout line. New frontends should prefer `nav --json-rpc`, which
wraps the same events in JSON-RPC 2.0 notifications and includes an explicit
protocol version.

## Transport

- Framing: newline-delimited JSON, one JSON-RPC object per line.
- Stdout: protocol notifications only.
- Stderr: human-readable diagnostics and startup context.
- Stdin: optional newline-delimited approval responses.
- JSON-RPC version: `"2.0"`.
- Protocol version: `1`, carried in every `params.protocol_version` object.

`--json-rpc` always runs headless. A frontend should launch it from the target
workspace cwd and pass the user prompt as the positional prompt:

```sh
nav --json-rpc "summarize the working tree"
```

Unlike raw `--json-events`, JSON-RPC mode does not read prompt text from piped
stdin. Stdin is reserved for protocol messages such as approval responses.

## Outbound Notifications

### `nav.session.started`

Emitted once before the agent turn begins.

```json
{
  "jsonrpc": "2.0",
  "method": "nav.session.started",
  "params": {
    "protocol_version": 1,
    "session_id": "01HZZZZZZZZZZZZZZZZZZZZZZZ",
    "cwd": "/Users/season/code/project",
    "model": "gpt-5.5",
    "transport": "websocket"
  }
}
```

### `nav.event`

Emitted once for every `AgentEvent` produced by `nav-core`.

```json
{
  "jsonrpc": "2.0",
  "method": "nav.event",
  "params": {
    "protocol_version": 1,
    "event": {
      "kind": "assistant_message_delta",
      "text": "Hello"
    }
  }
}
```

The `event` object is the same `AgentEvent` shape used by the TUI and raw
`--json-events` stream. Its `kind` field is the discriminant. Unknown future
`event.kind` values should be ignored or rendered as generic log rows.

Important event families:

- `user_message`: user prompt accepted by the agent loop.
- `assistant_message_delta`: transient streaming text.
- `assistant_message_done`: final assistant message text.
- `tool_call_started`, `tool_call_output`: tool lifecycle.
- `file_change`, `turn_diff`: structured mutation summaries.
- `tool_call_approval_request`: asks the frontend for a decision.
- `tool_call_blocked`: command or write refused before execution.
- `pending_input_*`, `turn_aborted`: queued follow-up and interruption flow.
- `compaction_*`, `context_trimmed`, `provider_retry`: long-session behavior.
- `turn_complete`, `error`: terminal turn states.

## Approval Responses

When a `nav.event` contains `event.kind: "tool_call_approval_request"`, the
frontend can answer on stdin using `nav.approval.respond`.

```json
{
  "jsonrpc": "2.0",
  "method": "nav.approval.respond",
  "params": {
    "approval_id": "01J...",
    "decision": "approved"
  }
}
```

Valid decisions are the values advertised by the request event's
`available_decisions` array. Current values are `approved`,
`approved_for_session`, `denied`, and `abort`.

For backward compatibility, stdin also accepts the legacy raw line:

```json
{"kind":"approval_response","approval_id":"01J...","decision":"approved"}
```

## Compatibility Rules

- `params.protocol_version` is the protocol compatibility key. A future
  incompatible contract must increment it.
- `nav.event.params.event` is append-only at the field level where possible.
  Consumers should ignore unknown fields.
- New `event.kind` variants may appear in protocol v1. Consumers should not
  treat unknown variants as fatal.
- Existing `event.kind` names and required fields are stable for protocol v1.
- Human text on stderr is not protocol data.
