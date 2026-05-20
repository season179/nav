# Six-Part Agent Harness Refactor

## Objective

Make `nav` easier to understand by restructuring `nav-core` around the six
clear parts of a good agent harness:

1. Tool registry
2. Model
3. Context management
4. Guardrails
5. Agent loop
6. Verify

The goal is not to make `nav` more abstract. The goal is to make the existing
behavior easier to find, audit, and explain. Preserve the educational feel:
small modules, plain names, explicit trust boundaries, and readable code.

## Implementation Status

This branch now moves the implementation behind the six reader-facing module
roots: `tool_registry`, `model`, `context`, `guardrails`, `agent_loop`, and
`verify`. The older flat module names remain as compatibility shims, so
downstream imports can keep compiling while new code has an obvious conceptual
home.

## Original Shape

Before this refactor, `nav` already had all six parts, but the source tree did
not make them obvious. The old crate root exposed a flat list of implementation
modules:

- `agent`
- `auth`
- `cli`
- `context_report`
- `control`
- `doctor`
- `extensions`
- `git_checkpoint`
- `git_diff`
- `models`
- `mutation`
- `permissions`
- `project`
- `protocol`
- `responses`
- `sandbox`
- `session`
- `skills`
- `tools`

This worked mechanically, but a new reader had to infer the harness shape from
many peer modules. The biggest reading burden is `agent/runner.rs`: it is the
right narrative center, but it also touches compaction, protected attachments,
model requests, tool execution, mutation reporting, turn diffs, session
persistence, subagents, and context-window recovery.

## Original Six-Part Map

### 1. Tool Registry

Original files:

- `crates/nav-core/src/tools/mod.rs`
- `crates/nav-core/src/tools/fs.rs`
- `crates/nav-core/src/tools/shell.rs`
- `crates/nav-core/src/tools/patch.rs`
- `crates/nav-core/src/tools/read_filter.rs`
- `crates/nav-core/src/tools/truncate.rs`
- `crates/nav-core/src/tools/output_accumulator.rs`

Responsibilities:

- Define model-visible tool schemas.
- Gate tools by agent scope with `ToolAccess`.
- Dispatch tool calls by name.
- Execute concrete adapters for filesystem, shell, patching, search, and
  subagent spawning.
- Shape tool output before feeding it back to the model.

Main clarity problem:

`tools/mod.rs` mixes registry, scope, dispatch, output types, permission
preflight integration, and a large test module. The Tool Registry exists, but
it is not named as a first-class module.

### 2. Model

Original files:

- `crates/nav-core/src/responses/mod.rs`
- `crates/nav-core/src/responses/request.rs`
- `crates/nav-core/src/responses/collector.rs`
- `crates/nav-core/src/responses/parser.rs`
- `crates/nav-core/src/responses/sse.rs`
- `crates/nav-core/src/responses/websocket.rs`
- `crates/nav-core/src/responses/retry.rs`
- `crates/nav-core/src/responses/types.rs`
- `crates/nav-core/src/models.rs`
- `crates/nav-core/src/auth.rs`

Responsibilities:

- Build Responses API request bodies.
- Hold provider transport adapters.
- Stream and collect model events.
- Extract assistant text, tool calls, raw output items, and usage.
- Warn about likely model-name typos.
- Load auth for ChatGPT OAuth or raw API-key mode.

Main clarity problem:

The model boundary is partly in `responses`, partly in `models`, partly in
`auth`, and partly in `agent/runner.rs` through the `ResponsesTransport` trait.
The request-builder also assembles context, so "model" and "context" are
currently tangled.

### 3. Context Management

Original files:

- `crates/nav-core/src/project.rs`
- `crates/nav-core/src/skills.rs`
- `crates/nav-core/src/extensions.rs`
- `crates/nav-core/src/session/mod.rs`
- `crates/nav-core/src/agent/replay.rs`
- `crates/nav-core/src/agent/compaction.rs`
- `crates/nav-core/src/context_report.rs`
- `crates/nav-core/src/responses/request.rs`

Responsibilities:

- Load project and user context files.
- Load settings and workspace status.
- Discover skills and prompt templates.
- Persist and replay session history.
- Estimate context size for `/context`.
- Compact long sessions.
- Decide what instructions, tools, replay items, and attachments go into the
  next model request.

Main clarity problem:

Context management is spread across project discovery, skills, session replay,
compaction, context reports, and request construction. The core concept is
"what will the next model call see?", but no module owns that question.

### 4. Guardrails

Original files:

- `crates/nav-core/src/permissions/mod.rs`
- `crates/nav-core/src/permissions/preflight.rs`
- `crates/nav-core/src/permissions/approval.rs`
- `crates/nav-core/src/permissions/classifier.rs`
- `crates/nav-core/src/permissions/dangerous.rs`
- `crates/nav-core/src/permissions/protected.rs`
- `crates/nav-core/src/permissions/external.rs`
- `crates/nav-core/src/permissions/safe_commands.rs`
- `crates/nav-core/src/permissions/bash_parse.rs`
- `crates/nav-core/src/sandbox/mod.rs`
- `crates/nav-core/src/sandbox/seatbelt.rs`
- `crates/nav-core/src/sandbox/passthrough.rs`
- `crates/nav-core/src/tools/fs.rs`
- `crates/nav-core/src/agent/runner.rs`

Responsibilities:

- Classify shell commands.
- Ask for approval when policy requires it.
- Block unbypassable or protected-metadata writes.
- Gate protected reads.
- Enforce filesystem path containment.
- Select and run the sandbox adapter.
- Handle attachment approval before a turn is emitted.

Main clarity problem:

The guardrails are strong, but they are split between `permissions`, `sandbox`,
filesystem helpers, and attachment logic inside the runner. Auditing the safety
story requires chasing several modules.

### 5. Agent Loop

Original files:

- `crates/nav-core/src/agent/runner.rs`
- `crates/nav-core/src/agent/events.rs`
- `crates/nav-core/src/control.rs`
- `crates/nav-core/src/protocol.rs`
- `crates/nav-cli/src/main.rs`
- `crates/nav-tui/src/turn.rs`

Responsibilities:

- Accept a user prompt.
- Optionally compact before the turn.
- Append the user message to model input.
- Call the model.
- Stream and emit events.
- Execute requested tools.
- Append function-call outputs.
- Loop until the model stops asking for tools.
- Emit durable `AgentEvent`s.
- Finalize the turn.

Main clarity problem:

The loop is conceptually simple, but the implementation is large because it
directly coordinates every other concern. The file should read like the
high-level harness loop; helper modules should carry the heavy detail.

### 6. Verify

Original files:

- `crates/nav-core/src/git_diff.rs`
- `crates/nav-core/src/mutation.rs`
- `crates/nav-core/src/doctor.rs`
- `crates/nav-core/src/agent/runner.rs`
- `crates/nav-core/src/tools/shell.rs`

Responsibilities:

- Summarize mutations from `edit_file` and `apply_patch`.
- Emit `FileChange` events.
- Collect `TurnDiff` after mutating tools.
- Let the model run tests through `bash`.
- Provide `nav doctor` for local environment checks.

Main clarity problem:

Verify exists as behavior, but not as a named harness part. Evidence after a
mutation is currently incidental to `finalize_turn`, `git_diff`, and tool
execution. It should be easier to see where `nav` records "what changed" and
"what evidence did we collect?"

## Proposed Target Shape

Keep the public crate stable where practical, but make the source tree read like
the harness:

```text
crates/nav-core/src/
  agent_loop/
    mod.rs
    runner.rs
    events.rs
    control.rs

  tool_registry/
    mod.rs
    definitions.rs
    dispatch.rs
    adapters/
      fs.rs
      shell.rs
      patch.rs
      read_filter.rs
      truncate.rs
      output_accumulator.rs

  model/
    mod.rs
    auth.rs
    names.rs
    responses/
      request.rs
      collector.rs
      parser.rs
      retry.rs
      sse.rs
      websocket.rs
      types.rs

  context/
    mod.rs
    project.rs
    skills.rs
    extensions.rs
    replay.rs
    compaction.rs
    report.rs
    session/
      mod.rs
      init.sql

  guardrails/
    mod.rs
    approval.rs
    preflight.rs
    permissions.rs
    sandbox/
      mod.rs
      seatbelt.rs
      passthrough.rs
      seatbelt.sbpl

  verify/
    mod.rs
    mutation.rs
    git_diff.rs
    doctor.rs
```

This exact tree does not have to land all at once. The important thing is the
reader-facing module names and the ownership of each concept.

## Deepening Opportunities

### 1. Make the six harness parts visible at the crate root

Files:

- `crates/nav-core/src/lib.rs`
- `crates/nav-core/src/agent.rs`
- New module roots matching the six harness parts.

Problem:

`lib.rs` currently exports implementation modules as peers. This is accurate
but not instructive. Readers see a pile of nouns instead of the agent harness
shape.

Solution:

Introduce the six top-level modules and re-export existing types through them.
This can start as a mostly mechanical move with compatibility exports preserved:

- `tool_registry`
- `model`
- `context`
- `guardrails`
- `agent_loop`
- `verify`

Benefits:

- High readability gain with low behavior risk.
- Gives future work an obvious destination.
- Makes docs and source agree.

Acceptance:

- A reader can open `nav-core/src/lib.rs` and immediately see the six parts.
- Existing downstream imports continue to compile or have obvious aliases.
- No behavior change.

### 2. Split tool registry from tool execution

Files:

- `crates/nav-core/src/tools/mod.rs`
- `crates/nav-core/src/tools/fs.rs`
- `crates/nav-core/src/tools/shell.rs`
- `crates/nav-core/src/tools/patch.rs`

Problem:

`tools/mod.rs` is doing too much: tool schemas, access policy, dispatcher,
tool outcome types, preflight integration, helper functions, and many tests.

Solution:

Create a `tool_registry` module with a small public interface:

- tool definitions
- tool access policy
- tool names/constants
- dispatch entrypoint

Move concrete implementations under `tool_registry/adapters/` or keep them as
private submodules behind the registry.

Benefits:

- The Tool Registry interface becomes the test surface.
- The dispatch trust boundary remains explicit.
- Adding a tool has one obvious front door.

Acceptance:

- Tool definitions and tool dispatch are still easy to audit.
- Existing tests pass after moving paths.
- No hidden dynamic registry unless there is a real second adapter/source.

### 3. Make context management own "what the model sees"

Files:

- `crates/nav-core/src/responses/request.rs`
- `crates/nav-core/src/project.rs`
- `crates/nav-core/src/skills.rs`
- `crates/nav-core/src/agent/replay.rs`
- `crates/nav-core/src/agent/compaction.rs`
- `crates/nav-core/src/context_report.rs`
- `crates/nav-core/src/session/mod.rs`

Problem:

Context is the most cross-cutting concept in the project. Request building,
project context, skills, replay, compaction, attachments, and `/context` all
answer parts of the same question, but no module owns the full question.

Solution:

Create a `context` module that owns:

- instruction section construction
- skill and project-context injection
- replay input construction
- attachment rendering
- compaction planning
- context report measurement

The `model` module should receive a prepared request context, not discover or
assemble it itself.

Benefits:

- `/context`, replay, compaction, and request building stay in sync.
- Context-window fixes have one home.
- The model layer becomes smaller and provider-focused.

Acceptance:

- `response_body` no longer directly knows how to discover or format every
  kind of context.
- `/context` measures the same parts the request builder sends.
- Existing replay and compaction tests still cover behavior.

### 4. Consolidate guardrail orchestration

Files:

- `crates/nav-core/src/permissions/*`
- `crates/nav-core/src/sandbox/*`
- `crates/nav-core/src/tools/preflight.rs`
- `crates/nav-core/src/tools/fs.rs`
- `crates/nav-core/src/agent/runner.rs`

Problem:

Safety behavior is correct but distributed. Permission policy, approval gates,
sandbox selection, protected-file checks, protected attachment gating, and
filesystem containment live in different places.

Solution:

Create a `guardrails` module that owns the high-level guardrail contract:

- evaluate tool preflight
- request/record approvals
- select sandbox policy
- gate protected attachments
- expose stable approval/block reason types

Keep low-level filesystem containment close to filesystem adapters, but route
its public safety contract through `guardrails`.

Benefits:

- Easier safety audit.
- Less safety logic inside the agent loop.
- Clearer relationship between approvals, sandboxing, and path guards.

Acceptance:

- The safety docs can point to one module first.
- Attachment gating leaves `runner.rs`.
- Existing path-security and approval tests still pass.

### 5. Move model-specific interfaces under `model`

Files:

- `crates/nav-core/src/responses/*`
- `crates/nav-core/src/models.rs`
- `crates/nav-core/src/auth.rs`
- `crates/nav-core/src/agent/runner.rs`

Problem:

The model seam is split: provider code is in `responses`, model-name heuristics
are in `models`, auth is separate, and the `ResponsesTransport` trait lives in
the agent runner.

Solution:

Create `model/` and move provider-specific code under it. The agent loop should
depend on a small model interface:

- submit a prepared request
- stream raw provider events
- surface usage and tool calls through parser helpers

Benefits:

- The agent loop no longer owns the provider abstraction.
- Future provider work has a clear home.
- The Model part becomes easy to explain independently from Context Management.

Acceptance:

- `ResponsesTransport` or its replacement lives outside `runner.rs`.
- OpenAI Responses details stay behind `model::responses`.
- Request context remains provided by `context`, not assembled ad hoc in model
  transport code.

### 6. Make verification a named harness part

Files:

- `crates/nav-core/src/git_diff.rs`
- `crates/nav-core/src/mutation.rs`
- `crates/nav-core/src/doctor.rs`
- `crates/nav-core/src/agent/runner.rs`
- `crates/nav-core/src/tools/shell.rs`

Problem:

Verify is real but implicit. `nav` records changed files and turn diffs, and
the model can run tests with `bash`, but the code does not give readers a
single place to understand how `nav` proves or summarizes work.

Solution:

Create a `verify` module for:

- mutation summaries
- turn diffs
- doctor checks
- future structured verification events

Do not force every verification into a rigid framework. Keep `bash` as the
general execution tool, but make post-mutation evidence gathering a named part
of the harness.

Benefits:

- Completes the six-part mental model.
- Makes `FileChange` and `TurnDiff` feel intentional.
- Gives observability work a natural destination.

Acceptance:

- `finalize_turn` delegates evidence collection instead of knowing git diff
  details.
- `mutation` and `git_diff` are discoverable through `verify`.
- No change in when `TurnDiff` is emitted.

## Recommended Order

### Phase 1: Reader-facing skeleton

Create the six modules and move/re-export the least risky pieces first.

Suggested first moves:

1. Add `tool_registry` as a wrapper around existing `tools`.
2. Add `verify` as a wrapper around existing `mutation`, `git_diff`, and
   `doctor`.
3. Update `lib.rs` exports and docs to show the six harness parts.

Why first:

This gives the biggest ease-of-understanding win without rewriting the loop.

### Phase 2: Split tool registry

Move tool definitions and access policy out of the dispatch implementation.
Keep the dispatch match explicit.

Why second:

The Tool Registry is the easiest part to make clear, and it gives the model
tool surface one obvious home.

### Phase 3: Extract verification from the runner

Move turn diff collection and mutation evidence formatting behind `verify`.
Keep behavior exactly the same.

Why third:

This makes the sixth part concrete and shrinks the runner without touching
provider or context complexity.

### Phase 4: Context management pass

Create a real `context` module that owns instruction sections, replay,
compaction, context reporting, project files, skills, and attachments.

Why fourth:

This is high value but higher blast radius. Do it after the simpler module
skeleton has landed.

### Phase 5: Model module pass

Move `ResponsesTransport`, OpenAI Responses transport, request submission,
collector, parser, retry, auth, and model-name helpers under `model`.

Why fifth:

This becomes cleaner after context assembly has moved out of request-building.

### Phase 6: Guardrails consolidation

Move high-level guardrail orchestration into `guardrails`, keeping low-level
path checks near filesystem operations.

Why sixth:

Guardrails are safety-sensitive. The refactor should be done once the target
module shape is stable and tests can prove no behavior drift.

## Non-Goals

- Do not introduce a plugin framework for tools.
- Do not make the registry dynamic unless there is a real second tool source.
- Do not hide the tool dispatch behind clever abstractions.
- Do not weaken path safety, approval behavior, sandbox behavior, or protected
  metadata rules.
- Do not change CalVer or release metadata.
- Do not rewrite the TUI as part of this refactor.
- Do not change the JSON-RPC or `AgentEvent` wire format unless a move proves
  a small compatibility-safe cleanup is necessary.

## Verification Plan

Run the normal Rust validation bundle after each phase:

```sh
cargo fmt
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

For docs or architecture-guide changes:

```sh
git diff --check
```

For TUI-visible changes, use the existing `tmux`-driven manual smoke pattern
before claiming the UI still works.

## Success Criteria

The refactor is successful when:

- `nav-core/src/lib.rs` reveals the six harness parts directly.
- A new reader can answer "where are tools defined?", "where does the model
  call happen?", "where is context assembled?", "where are safety rules?", "where
  is the loop?", and "where is verification evidence gathered?" without reading
  the whole crate.
- `agent_loop/runner.rs` reads primarily as the model/tool loop, not as a
  warehouse for every cross-cutting concern.
- Tool schemas, model request construction, context management, guardrails,
  loop orchestration, and verification evidence each have a clear home.
- Existing behavior, session replay, compaction, approvals, sandboxing, path
  safety, and frontend event contracts remain unchanged unless explicitly
  called out.
