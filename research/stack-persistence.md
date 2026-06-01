# Stack Persistence

The stack view should distinguish between data that can be reconstructed from
stored turns and data that must be captured at the live model-call boundary.

## Replay Boundary

These stack details cannot be derived exactly from a DB replay today:

- Raw provider response JSON. Stored assistant and tool turns keep the normalized
  content, reasoning text, and tool calls/results, but not the original response
  body.
- Provider request/response metadata: request id, response id, HTTP status,
  provider model id, provider error payloads, and similar transport details.
- Exact historical provider request payload. Replay can rebuild an approximate
  request, but system prompt assembly, context-file contents, tool schemas,
  config, model settings, and adapter behavior may have changed.
- Exact system prompt and project-context snapshot loaded for that call.
- Exact tool definitions advertised to the provider for that call.
- Per-model-call timing. The DB stores run timing, not individual model-call
  timing inside multi-call runs.
- Per-model-call token usage, source, and confidence. Session token columns are
  aggregate counters.
- Non-obvious finish reason. Tool-call finishes are inferable from tool calls,
  but plain replies do not preserve stop versus length versus provider-specific
  finish reasons.
- Cancel-after-response hidden output. A model response can exist in the stack
  even when cancellation prevents the assistant turn from being emitted.
- Queued steering that is dropped on cancel before it is drained and persisted.
- Stack-specific call id, status detail, and error strings.

## Raw Provider Payloads

Normalization is intentionally lossy. The OpenAI-compatible parser keeps only
the fields required by `ModelResponse`:

- assistant content
- assistant reasoning content when exposed as `reasoning_content`
- tool call id, function name, and function arguments
- normalized finish reason
- selected token usage counters

Everything else in the raw request/response is lost unless the `ProviderCallTrace`
is captured separately: provider object ids, created timestamps, choices beyond
the first, role/name annotations outside the normalized message shape, logprobs,
refusal or annotation payloads, service tier, system fingerprint, arbitrary
provider extensions, full usage breakdowns not mapped by `parse_token_usage`,
HTTP status, request id, response id, raw error bodies, and exact provider
request JSON.
