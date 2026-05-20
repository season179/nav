# OTel Observability PRD

## Status

Proposed. This PRD records the product and engineering shape only. Do not
implement it as part of this document-writing pass.

Research currency: checked on 2026-05-20 against current OpenTelemetry,
Phoenix, and Langfuse documentation.

## Problem Statement

`nav` needs a way to understand how it is actually used so the product can
improve from evidence instead of vibes. The local session log already records
the canonical transcript and agent events, but it is not enough by itself for
cross-session questions:

- Which workflows happen most often?
- Which tools are slow, noisy, blocked, or repeatedly retried?
- Which turns burn the most input tokens?
- Which model/tool/error patterns precede user corrections or aborts?
- How often do approvals, compactions, context trims, retries, file edits, and
  failed commands happen?
- Which backend or frontend choices make `nav` feel better or worse?

At the same time, `nav` should not become locked to a single LLM observability
vendor. Phoenix is attractive as an open-source local workbench. Langfuse is
attractive as a managed product-analytics and LLM-observability layer. Both can
receive OpenTelemetry traces, but they do not interpret every span the same way.

The product need is therefore not "choose Phoenix or Langfuse forever." The
need is a durable telemetry design where `nav` emits useful, portable,
backend-neutral traces while still adding enough GenAI/OpenInference semantics
for LLM-specific backends to render those traces well.

## Current Facts

- `nav` already has a first-class `AgentEvent` stream for user messages,
  assistant messages, tool calls, subagents, file changes, approvals, blocked
  tools, pending input, turn completion, aborts, provider retries, context
  trimming, compaction lifecycle, and errors.
- `TurnComplete` already carries normalized usage counters:
  `tokens_input`, `tokens_output`, `tokens_input_cached`, and
  `tokens_reasoning`.
- The SQLite session store persists durable events and increments session token
  rollups on `TurnComplete`.
- The workspace currently has no direct OpenTelemetry dependency in
  `Cargo.toml`.
- OpenTelemetry GenAI semantic conventions are still marked as development in
  the upstream docs.
- Phoenix uses OpenInference semantic conventions as its standard display
  format and documents translation from other GenAI conventions into
  OpenInference.
- Langfuse can ingest OTLP traces, maps OTel spans into its own trace and
  observation data model, and recommends some `langfuse.*` attributes for
  manually instrumented applications when filterability matters.

## Product Decision

Use OpenTelemetry/OTLP as the export pipe, but do not treat OTel alone as the
whole observability product.

`nav` should emit three layers of attributes:

1. **Stable `nav.*` attributes** for product truth that should survive backend
   changes.
2. **OpenTelemetry GenAI attributes** where they map cleanly to model, token,
   tool, and agent concepts.
3. **OpenInference attributes** where they are needed for Phoenix-quality LLM
   rendering.

Backend-specific attributes such as `langfuse.*` may be added narrowly when
they unlock important Langfuse behavior, but they must not become the canonical
schema for `nav`.

The local SQLite `AgentEvent` log remains the source of truth. Telemetry is a
query and debugging lens, not the durable audit log.

## Goals

1. Track normal `nav` usage across sessions and turns.
2. Make slow, expensive, failed, blocked, retried, aborted, and corrected turns
   easy to find.
3. Keep backend switching cheap between Phoenix, Langfuse, and other
   OTLP-compatible systems.
4. Render useful LLM traces in Phoenix and Langfuse, not just generic spans.
5. Preserve local-first behavior when telemetry is disabled.
6. Avoid making telemetry export failure affect the agent loop.
7. Avoid storing raw secrets, huge tool outputs, or complete diffs only in the
   observability backend.
8. Keep full replay/audit data in the existing session log or local artifacts.
9. Make the telemetry schema explicit and versioned.
10. Make the first implementation testable without a live Phoenix or Langfuse
    server.

## Non-Goals

- Do not implement telemetry in this PRD pass.
- Do not replace the SQLite session log.
- Do not make Phoenix, Langfuse, or any cloud service required to run `nav`.
- Do not build a full logs/metrics/APM platform inside `nav`.
- Do not store unbounded raw shell output, raw diffs, or full transcripts in
  spans by default.
- Do not compute dollar costs from stale static pricing tables. Cost should
  come from provider-reported cost when available; otherwise token usage is the
  reliable baseline.
- Do not commit to one exact OTel GenAI semantic convention version until
  implementation time.
- Do not add dashboards, eval jobs, or alerting in the first slice.

## User Stories

1. As a `nav` user, I want to see which sessions and turns used the most
   tokens, so I can understand where context is being spent.
2. As a `nav` user, I want to inspect slow tool calls, so I can find commands
   or workflows that make the agent feel stuck.
3. As a `nav` user, I want failed and blocked tool calls to be visible, so I
   can improve permissions, tool design, or model instructions.
4. As a `nav` user, I want compaction and context-trimming events visible, so I
   can understand when long-session behavior changes.
5. As a `nav` user, I want retries and provider failures visible, so transport
   issues are not confused with model quality.
6. As a `nav` user, I want turn traces grouped by session, so I can follow a
   real task over time.
7. As a `nav` user, I want to compare models, transports, frontends, approval
   policies, and sandbox modes, so I can choose better defaults.
8. As a `nav` user, I want to send the same telemetry to Phoenix or Langfuse,
   so the backend choice stays reversible.
9. As a maintainer, I want deterministic span snapshots in tests, so schema
   drift is caught before it reaches a backend.
10. As a maintainer, I want full raw data to remain recoverable from the
    session log, so bounded telemetry does not destroy debugging fidelity.
11. As a future frontend implementer, I want telemetry to use the same
    `AgentEvent` concepts as the protocol, so terminal, script, and chat
    experiments can be compared consistently.
12. As a privacy-conscious future user, I want telemetry disabled by default or
    clearly configured, so `nav` does not unexpectedly export workspace data.

## Proposed Solution

Add a small telemetry layer in `nav-core` that observes the agent lifecycle and
emits OpenTelemetry traces through OTLP when enabled.

The first design should prefer direct OTLP export. A Collector can be added
later for buffering, fan-out, filtering, redaction, or multi-backend routing.

The telemetry layer should be fed from the same places that emit `AgentEvent`
instead of inventing a separate product event model. The `AgentEvent` log stays
canonical; spans are derived operational views over the same lifecycle.

### Trace Shape

Use one trace per user-visible turn.

The root span should be:

```text
nav.turn
```

A session should group traces through attributes, not by keeping one giant trace
open for the whole session. This keeps each turn queryable, bounded, and easier
to export.

Expected child spans:

- `nav.context.build`
- `nav.model.request`
- `nav.tool.call`
- `nav.shell.command`
- `nav.approval.wait`
- `nav.file.edit`
- `nav.compaction`
- `nav.provider.retry`
- `nav.subagent.run`
- `nav.git.checkpoint`

Not every `AgentEvent` needs a span. Stream deltas should usually remain
events or counters, not thousands of spans.

### Stable `nav.*` Attributes

These attributes are the product schema. They should be documented and versioned
independently of any backend.

Root turn attributes:

```text
nav.telemetry.schema_version
nav.session_id
nav.turn_id
nav.frontend
nav.cwd
nav.model.requested
nav.model.effective
nav.auth_mode
nav.transport
nav.approval_policy
nav.sandbox
nav.resume
nav.compacted_since_start
```

Usage attributes:

```text
nav.usage.tokens_input
nav.usage.tokens_output
nav.usage.tokens_input_cached
nav.usage.tokens_reasoning
nav.usage.cost_usd
nav.usage.cost_source
```

Tool attributes:

```text
nav.tool.call_id
nav.tool.name
nav.tool.is_error
nav.tool.requires_approval
nav.tool.approval_decision
nav.tool.block_rule
nav.tool.output_truncated
nav.tool.output_spilled
nav.tool.output_artifact_id
```

Context attributes:

```text
nav.context.input_items
nav.context.estimated_tokens
nav.context.trimmed_pairs
nav.context.compaction_trigger
nav.context.compaction_replaced_events
```

Mutation attributes:

```text
nav.file.change_count
nav.file.changed_paths
nav.git.checkpoint_action
nav.git.checkpoint_status
```

Use care with high-cardinality attributes. IDs such as `session_id`, `turn_id`,
and `tool.call_id` are useful for linking back to the local log. Large free-form
text, full paths, command output, and diffs should be bounded or omitted.

### GenAI and OpenInference Mapping

For the model request span, map stable `nav` data into the current OTel GenAI
and OpenInference concepts at implementation time.

Expected intent:

- Mark the root turn as an agent operation.
- Mark model calls as LLM/generation spans.
- Mark tool calls as tool spans.
- Set model name, provider/system, token usage, input/output excerpts, and
  error status where the conventions support it.
- Set `session.id` or equivalent standard attributes so backends can group
  turns by session.
- Set OpenInference span kind values where Phoenix needs them for display.

Do not assume generic OTel spans will render well in LLM backends. The backend
can ingest the trace but still fail to show the right LLM/tool/agent UI if the
semantic attributes are missing or named incorrectly.

### Payload Policy

Telemetry payloads should be useful but bounded.

Default behavior:

- Include prompt/output excerpts only when explicitly enabled or below a small
  byte limit.
- Include structured summaries for tool outputs rather than full raw output.
- Include artifact or session-log references when full output exists locally.
- Include token counts and latency by default.
- Include tool names, status, truncation/spill flags, and error status by
  default.
- Do not put API keys, `.env` contents, PEM/private-key material, or shell
  secret values into spans.

Even when Season is the only user and privacy is not the main concern, secret
leakage is still a product bug. Telemetry should treat secrets differently from
ordinary user text.

### Export Configuration

Add explicit configuration later. The rough shape should support:

```json
{
  "telemetry": {
    "enabled": true,
    "endpoint": "http://localhost:4318",
    "protocol": "otlp_http",
    "headers_env": "OTEL_EXPORTER_OTLP_HEADERS",
    "backend_hint": "phoenix",
    "include_inputs": false,
    "include_outputs": false,
    "payload_max_bytes": 4096,
    "sample_rate": 1.0
  }
}
```

The exact config surface should follow existing `.nav/settings.json` and CLI
patterns when implementation starts.

Important backend notes:

- Phoenix quality depends on OpenInference-compatible attributes.
- Langfuse can receive OTLP over HTTP and maps OTel attributes into its trace
  and observation data model. Some attributes are only easily filterable if
  emitted with Langfuse's preferred metadata prefixes.
- A Collector is useful later, but direct OTLP keeps the first slice simpler.

## Tradeoffs

### Portable Pipe vs Backend UX

OTLP makes export portable, but it does not make Phoenix and Langfuse
equivalent. The same spans can be accepted by both systems while producing
different UI quality, filters, cost displays, and eval workflows.

Decision: keep the base schema backend-neutral, then add explicit semantic
mapping for important LLM backends.

### `nav.*` Schema vs Standard Conventions

Standard conventions reduce vendor lock-in. Custom `nav.*` attributes preserve
product facts that the standards do not cover yet.

Decision: use both. `nav.*` is stable for `nav`; OTel/OpenInference attributes
are interoperability views.

### Rich Payloads vs Safety and Cost

Raw prompts, outputs, diffs, and shell logs make debugging easier. They also
increase trace size, storage, backend clutter, and secret exposure.

Decision: send bounded excerpts and links/IDs by default; keep full details in
the local session log or local artifacts.

### Direct Export vs Collector

Direct export is easier to understand and test. A Collector gives better
buffering, filtering, fan-out, and backend routing.

Decision: start direct. Add Collector documentation after the schema proves
itself.

### Trace Per Turn vs Trace Per Session

A single session trace mirrors the human task, but long-running traces become
large and awkward. One trace per turn is bounded and queryable, but needs
session attributes to connect the story.

Decision: one trace per turn, grouped by `nav.session_id` and `session.id`.

### Manual Rust Instrumentation vs Vendor SDK

Vendor SDKs have better defaults for their own backend. Manual OTel fits Rust
and keeps `nav` in control of its agent-specific lifecycle.

Decision: manual Rust instrumentation first. Backend SDK helpers can be
considered later only if they do not take over the product schema.

## Acceptance Criteria

When this PRD is implemented later:

1. With telemetry disabled, `nav` performs no telemetry network export.
2. With telemetry enabled and a test exporter, a normal turn emits one
   `nav.turn` root span.
3. Model calls, tool calls, approvals, retries, compactions, aborts, and file
   edits appear as child spans or span events with stable `nav.*` attributes.
4. `TurnUsage` counters are exported on the correct span.
5. Every exported turn can be linked back to the local session log by
   `nav.session_id`, `nav.turn_id`, and relevant event or tool IDs.
6. Export failure is visible in local diagnostics but does not fail the agent
   turn.
7. Payload caps are enforced in tests.
8. Secret-like values are not exported in default configurations.
9. Phoenix can render a smoke-test trace as an agent/model/tool trace.
10. Langfuse can ingest a smoke-test trace and group/filter by session.
11. The docs explain how to point the same instrumentation at Phoenix,
    Langfuse, or an OTel Collector.

## Testing Decisions

- Add an in-memory test exporter so most telemetry tests do not require a live
  backend.
- Snapshot the span tree for representative turns.
- Unit-test the mapping from `AgentEvent`/turn lifecycle to span names and
  attributes.
- Test payload truncation, spill references, and redaction.
- Test disabled telemetry to prove no exporter is initialized.
- Test exporter failure paths.
- Add one manual smoke checklist for Phoenix and one for Langfuse, but do not
  make live hosted services mandatory for CI.

Representative fixtures should cover:

- successful assistant-only turn
- model turn with one tool call
- failed shell command
- blocked dangerous command
- approval request and denial
- file edit and turn diff
- provider retry
- compaction start/completion/failure
- turn abort
- large/truncated tool output

## Rollout Plan

1. Finalize the telemetry schema and span tree in this PRD.
2. Add a no-op telemetry facade and in-memory exporter tests.
3. Instrument root turn spans and model request spans.
4. Instrument tool calls, approvals, retries, file edits, compaction, and aborts.
5. Add direct OTLP export configuration.
6. Smoke-test Phoenix locally.
7. Smoke-test Langfuse with OTLP HTTP.
8. Document optional Collector fan-out after direct export works.

## Open Questions

- Which exact Rust OpenTelemetry crate versions should be used at
  implementation time?
- Should telemetry be disabled by default, or enabled only when an endpoint is
  configured?
- Should raw user prompts ever be exported by default for a sole-user local
  build?
- How should command paths be normalized to avoid turning every workspace path
  into high-cardinality metadata?
- Should `nav` expose a local command to print the telemetry schema version and
  current config?
- Should cost be represented only when provider-reported, or should there be an
  explicit `unknown` value even when token usage exists?
- Should Phoenix/OpenInference and Langfuse metadata mappings live in the same
  exporter or in separate span processors?

## References

- Local event model: `crates/nav-core/src/agent/events.rs`
- Local session persistence: `crates/nav-core/src/session/mod.rs`
- Workspace dependencies: `Cargo.toml`
- OpenTelemetry GenAI semantic conventions:
  https://opentelemetry.io/docs/specs/semconv/gen-ai/
- Phoenix semantic convention translation:
  https://arize.com/docs/phoenix/tracing/concepts-tracing/translating-conventions
- Langfuse OpenTelemetry ingestion and attribute mapping:
  https://langfuse.com/integrations/native/opentelemetry

## Further Notes

OTel makes the telemetry pipe portable. It does not make every backend equally
good for `nav`.

The important product bet is to make `nav`'s own lifecycle observable in a
stable way, then project that lifecycle into Phoenix, Langfuse, or another OTLP
backend. That keeps the expensive work in the right place: careful
instrumentation of `nav`, not rewrites for each observability vendor.
