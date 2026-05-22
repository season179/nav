# OpenAI-compatible provider support — issue drafts

19 issues to take nav from "OpenAI + ChatGPT subscription only" to "any
OpenAI-compatible Chat Completions endpoint via a configurable provider
catalog." Codex/ChatGPT subscription mode is the only Responses API consumer
and stays as-is.

## Difficulty tags

- `needs-opus` — judgement-heavy, cross-module, or design-sensitive. Hand to
  Opus or equivalent.
- `weak-model-ok` — mechanical, scoped, test-driven. Safe for Qwen3.7,
  DeepSeek-V4-Pro, GLM-5.1.

## Dependency map

```
G3 value resolver ────┐
G1 schema ────────────┼──► G2 resolution ──┬──► G4 built-ins
                      │                    ├──► G5 reasoning + flag
                      │                    ├──► G6 /model (restart)
                      │                    ├──► G7 nav models/providers
                      │                    ├──► G8 nav doctor diagnostics
                      │                    └──► G9 auth auto-detect
                      │
                      └──► F1 chat-completions module
                              ├──► C1 request builder ─┐
                              ├──► C2 response parser ─┼─► first turn works
                              ├──► C3 tool wrapper   ──┘
                              ├──► C4 overflow detect
                              ├──► C5 typo guard quiet
                              ├──► C6 SSE fixtures ──► F2 SSE normalizer
                              └──► F3 history convert ──► F4 /model live-swap
                              
                              ──► D1 provider cookbook
                              ──► D2 auth migration note
```

Filing order: G1, G3 in parallel, then G2, then everything else.

---

## G1. Provider/model schema in settings.json

**Labels**: `area/config`, `weak-model-ok`

### Summary

Extend `.nav/settings.json` / `~/.nav/settings.json` with a nested `providers`
catalog and a `default_model` selector. No behavior change yet — this issue
adds the types, parsing, and validation only.

### Background

Today's `Settings` struct in `crates/nav-core/src/context/project.rs:144` only
carries `model: Option<String>`, `auth`, `transport`, etc. We need a place to
declare arbitrary OpenAI-compatible providers (z.ai, OpenRouter, Ollama, vLLM,
DeepSeek) with the same model name appearing under multiple providers. Pi's
shape (`../pi/packages/coding-agent/src/core/model-registry.ts:184`) is the
precedent — adopt it directly: providers at the top level, models nested
under each provider, `default_model` qualified as `<provider_id>/<model_name>`.

### Schema

```json
{
  "providers": {
    "z.ai": {
      "name": "Z.AI",
      "base_url": "https://api.z.ai/v1",
      "api_key": "ZAI_API_KEY",
      "headers": { "X-Custom": "value" },
      "models": {
        "glm-5.1": {
          "model_id": "glm-5.1",
          "reasoning_effort": "high",
          "max_output_tokens": 16384
        }
      }
    }
  },
  "default_model": "z.ai/glm-5.1"
}
```

Field rules:
- `name` optional, defaults to the provider id (used for `/model` display).
- `base_url` required when the provider isn't a built-in (G4).
- `api_key` optional (Ollama doesn't need one). Resolution semantics live in G3.
- `headers` optional, values resolved through G3.
- Per-model `model_id` defaults to the model key if omitted.
- `reasoning_effort` is `"low" | "medium" | "high"` and optional.
- `default_model` must parse as `<provider>/<model>` and reference a real entry.

### Files

- `crates/nav-core/src/context/project.rs` — extend `Settings`, the merge
  function, and `read_settings`.
- `crates/nav-core/src/context/mod.rs` — re-export new types if needed.

### Acceptance criteria

- [ ] New types compile and round-trip through serde.
- [ ] `deny_unknown_fields` on every new struct.
- [ ] Project-over-user merge is **shallow** at the provider id level (a
      project entry fully replaces the user entry with the same id rather
      than deep-merging fields). Document this in the doc comment.
- [ ] `default_model` value is validated against the merged catalog at load
      time; a dangling reference logs a single stderr line and is ignored
      (not fatal — nav must still start so the user can fix the file).
- [ ] Existing `model: Option<String>` field stays untouched (compatibility).

### Test plan

- Unit test: full nested catalog round-trips.
- Unit test: project providers replace user providers with the same id.
- Unit test: `default_model` referencing a missing entry logs and falls back.
- Unit test: `deny_unknown_fields` catches typos like `"baseUrl"` vs
  `"base_url"`.

### Out of scope

- Resolving the catalog into a usable `ResolvedProvider` (G2).
- Actually using `default_model` to pick the model at startup (G2/G9).
- Built-in catalog entries (G4).

---

## G2. Provider/model resolution at startup

**Labels**: `area/config`, `area/model`, `weak-model-ok`

### Summary

Add a resolution layer that turns `--model <provider>/<name>` (or
`default_model` from G1) into a `ResolvedProvider { base_url, bearer, headers,
display_name, model_id, reasoning_effort, max_output_tokens }`. Replaces the
api-key half of `crates/nav-core/src/model/auth.rs:39`.

### Background

`load_auth` today only knows two shapes: `ApiKey` (hard-coded
`api.openai.com`) and `Chatgpt` (hard-coded Codex backend). With a provider
catalog, the api-key path becomes a lookup: read `args.model`, find the
provider, resolve the api key via G3, build a `ResolvedProvider`. Codex/ChatGPT
mode is unaffected by this issue — keep that branch exactly as it is.

### Files

- `crates/nav-core/src/model/auth.rs` — add `ResolvedProvider` and a resolver
  function alongside the existing `load_auth`. Don't delete the old one yet
  (G9 finishes the migration).
- `crates/nav-core/src/cli/mod.rs:46` — accept `<provider>/<model>` shape in
  `--model`.

### Acceptance criteria

- [ ] New `ResolvedProvider` struct with `base_url`, `bearer: Option<String>`,
      `headers`, `model_id`, `reasoning_effort: Option<ReasoningEffort>`,
      `max_output_tokens: Option<u32>`, `display_name`.
- [ ] Resolver picks the entry from the merged catalog and produces a
      `ResolvedProvider`, or returns a clear error pointing at
      `nav providers list` (G7).
- [ ] When `--model` is a bare string (no `/`), the resolver tries to match
      it against any model whose qualified id ends with `/<bare>`; if there
      is exactly one match it wins, ambiguous matches return an error
      listing the candidates.
- [ ] When `--model` is omitted entirely, `default_model` is used.
- [ ] Resolver does NOT touch the Codex/ChatGPT path — that auth mode bypasses
      the catalog.

### Test plan

- Unit test: qualified `<provider>/<model>` resolves correctly.
- Unit test: bare `<model>` resolves when unambiguous, errors when not.
- Unit test: missing provider returns an actionable error message.
- Unit test: Codex auth mode is untouched.

### Out of scope

- The `--reasoning-effort` flag override (G5).
- Built-in catalog (G4).
- Falling back across providers when a credential is missing (G9).

---

## G3. Pi-style config value resolver

**Labels**: `area/config`, `weak-model-ok`

### Summary

Add a single function that resolves a config string into a runtime value with
three semantics, in this order:

1. If the string starts with `!`, execute the rest as a shell command and
   return stdout. Cached per process.
2. If `env::var(value)` returns a non-empty value, return that.
3. Otherwise return the value as a literal.

Used by `api_key` and any `headers` value in G1.

### Background

Pi's `../pi/packages/coding-agent/src/core/resolve-config-value.ts` is the
reference. The `!shellcmd` form handles macOS Keychain
(`!security find-generic-password -ws 'nav-openrouter'`), 1Password
(`!op read 'op://Personal/nav/key'`), `pass`, `bws`, etc. — one mechanism,
arbitrary secret backends.

### Files

- `crates/nav-core/src/model/auth.rs` — new module-local or sibling file
  `resolve_value.rs`.

### Acceptance criteria

- [ ] Pure function `resolve_value(input: &str) -> Result<Option<String>>`
      returning `Ok(None)` only when the lookup failed in a recoverable way
      (e.g., env var not set and command resolution wasn't requested);
      shell-command failure returns `Err`.
- [ ] Shell commands run via `std::process::Command::new("sh").args(["-c",
      cmd])` with a 10s timeout, stdin closed, stderr captured and surfaced
      in the error message.
- [ ] Shell-command results cached in a process-lifetime `Mutex<HashMap>`.
- [ ] Empty stdout from a shell command is treated as failure.
- [ ] Disambiguation rule: env var wins over literal. A literal `sk-...` key
      that happens to match an env var name is shadowed — document this in
      the doc comment.

### Test plan

- Unit test: literal string round-trips.
- Unit test: env var resolution (set + read in test).
- Unit test: `!echo hello` returns `"hello"`.
- Unit test: `!false` returns an error.
- Unit test: cache: `!date` invoked twice returns the same string.
- Unit test: empty stdout is an error.

### Out of scope

- Per-call cache bust (process restart is enough).
- Concurrent resolution races (the process is single-threaded for startup
  resolution anyway).

---

## G4. Built-in provider catalog

**Labels**: `area/config`, `weak-model-ok`

### Summary

Ship a fixed set of built-in providers so a user with only `OPENAI_API_KEY`
exported can run `nav` with zero config. User/project `settings.json` entries
override built-ins by id.

### Background

Without built-ins, every new user has to write a JSON catalog before nav
works. Built-ins make the zero-config path trivial.

### Files

- `crates/nav-core/src/context/project.rs` — built-in catalog as a `const fn`
  or `LazyLock<HashMap>` that `Settings::merge` layers user config over.

### Built-in entries

| id           | name              | base_url                              | api_key env       |
|--------------|-------------------|---------------------------------------|-------------------|
| `openai`     | OpenAI            | `https://api.openai.com/v1`           | `OPENAI_API_KEY`  |
| `openrouter` | OpenRouter        | `https://openrouter.ai/api/v1`        | `OPENROUTER_API_KEY` |
| `deepseek`   | DeepSeek          | `https://api.deepseek.com/v1`         | `DEEPSEEK_API_KEY` |
| `groq`       | Groq              | `https://api.groq.com/openai/v1`      | `GROQ_API_KEY`    |
| `together`   | Together          | `https://api.together.xyz/v1`         | `TOGETHER_API_KEY` |
| `zai`        | Z.AI              | `https://api.z.ai/v1`                 | `ZAI_API_KEY`     |
| `ollama`     | Ollama (local)    | `http://localhost:11434/v1`           | (none)            |
| `vllm`       | vLLM (local)      | `http://localhost:8000/v1`            | (none)            |

No built-in `models` map — users add the models they actually want. The
provider entry alone makes `--model openai/gpt-5.5` work without any
config file.

### Acceptance criteria

- [ ] Merged catalog contains all built-ins by default.
- [ ] A user `providers.openai = { ... }` entry fully replaces the built-in
      (consistent with G1's shallow merge).
- [ ] A user can add an entirely new provider id alongside built-ins.
- [ ] Removing a built-in is not supported (a user would just override
      `base_url` to something inert if they really wanted).

### Test plan

- Unit test: empty user settings → all built-ins present.
- Unit test: user override of `openai.base_url` wins.
- Unit test: adding `providers.custom` keeps built-ins intact.

### Out of scope

- A `built_in: false` flag to drop built-ins entirely.

---

## G5. Reasoning effort field + `--reasoning-effort` flag

**Labels**: `area/config`, `area/model`, `weak-model-ok`

### Summary

Plumb `reasoning_effort` from the model entry through to the request body,
and add a `--reasoning-effort {low,medium,high}` CLI flag that overrides
the configured value for a single run.

### Background

OpenAI reasoning models (gpt-5*, o-series) accept
`reasoning: { effort: "high" }` on the Responses API. Several Chat
Completions providers ship their own knob:

- DeepSeek-R1: `reasoning_effort` in the request body.
- z.ai/glm-5.1: `thinking: { type: "enabled" }`.
- Qwen3.7: chat-template flag.

This issue only wires the **field through the request builder** for OpenAI
Chat Completions (`reasoning_effort` at the top level). Other providers'
shapes are handled by their respective per-provider hooks (out of scope here
— file follow-ups when those providers become important).

### Files

- `crates/nav-core/src/cli/mod.rs:44` — new `--reasoning-effort` flag.
- `crates/nav-core/src/cli/mod.rs:242` — new `ReasoningEffort` enum.
- `crates/nav-core/src/model/auth.rs` — `ResolvedProvider.reasoning_effort`.
- Future `crates/nav-core/src/model/chat_completions/request.rs` (built in
  C1) — emits the field.

### Acceptance criteria

- [ ] `ReasoningEffort` enum: `Low`, `Medium`, `High`. ValueEnum + Deserialize.
- [ ] CLI flag layered correctly via the existing `ProvidedArgs` precedence:
      CLI > project > user > built-in.
- [ ] When the resolved value is `None`, the request body omits the field
      entirely (don't send `reasoning_effort: null`).
- [ ] No effect on Codex/ChatGPT path.

### Test plan

- Unit test: flag overrides model entry.
- Unit test: model entry value used when flag absent.
- Snapshot test of the Chat Completions request body with and without effort.

### Out of scope

- Provider-specific thinking shapes (z.ai, qwen). File as follow-ups.
- Per-model thinking-format detection.

---

## G6. `/model` slash command (restart-required variant)

**Labels**: `area/tui`, `area/config`, `weak-model-ok`

### Summary

Add `/model` to the TUI. With no argument it lists available models from the
merged catalog. With `<provider>/<name>` or a bare `<name>` it writes the
choice to a session-local override and asks the user to restart for it to
take effect.

### Background

Existing slash command pattern lives in
`crates/nav-tui/src/input/commands.rs:153` (`parse_builtin_command`) and
`crates/nav-tui/src/input/slash.rs`. The "restart required" variant is the
cheap first step — live swap (F4) reuses the same UX once the wire-format
plumbing is in place.

### Behavior

```
/model
  → ◆ models  3 configured
    > openai/gpt-5.5        OpenAI
      openrouter/zai/glm-5.1  OpenRouter
      ollama/qwen-local     Ollama (local)
    Current: openai/gpt-5.5
    Default: openai/gpt-5.5

/model openrouter/zai/glm-5.1
  → ◆ models  Set next session model to "openrouter/zai/glm-5.1".
    Restart nav (Ctrl+C, rerun) for the change to take effect.

/model glm-5.1
  → ◆ models  "glm-5.1" matches: openrouter/zai/glm-5.1
    Set next session model. Restart nav to apply.

/model unknown
  → ◆ models  No model matches "unknown". Run `/model` to list.
```

### Files

- `crates/nav-tui/src/input/commands.rs` — handler stanza alongside
  `/context`, `/resume`, etc.
- `crates/nav-tui/src/input/slash.rs` — recognize as a control command.
- `crates/nav-tui/src/bottom_pane/slash_popup.rs:61` — add to
  `builtin_description`.

### Acceptance criteria

- [ ] Listing groups models by provider and shows the display name.
- [ ] Selection accepts qualified and bare ids (using G2's disambiguation).
- [ ] Selection writes to a session preference file or a small per-session
      state file; restart picks it up via the existing settings precedence.
- [ ] Unknown model prints an actionable message without crashing.
- [ ] Slash popup autocompletes `/model` like other built-ins.

### Test plan

- Unit tests on the parse + match logic (no TUI render).
- Snapshot test of the listing output via `insta`.

### Out of scope

- Mid-session swap without restart (F4).
- Mid-session reasoning-effort swap.

---

## G7. `nav models` and `nav providers` subcommands

**Labels**: `area/cli`, `weak-model-ok`

### Summary

CLI mirror of `/model` listing: `nav models list` and `nav providers list`.
Useful for scripting and for `nav doctor` to call into.

### Files

- `crates/nav-core/src/cli/commands.rs:26` — add `Models { action }` and
  `Providers { action }` variants alongside the existing `Doctor` variant.
- New `crates/nav-core/src/cli/commands/models.rs` and `providers.rs` to keep
  per-command files small.

### Acceptance criteria

- [ ] `nav models list` prints one line per model: `<provider>/<name>  [provider display name]  [reasoning_effort if set]`.
- [ ] `nav providers list` prints one line per provider: `<id>  <display_name>  <base_url>  [credential resolvable: yes|no]`.
- [ ] `--json` flag on both for machine-readable output.
- [ ] No network calls; everything resolves locally from the catalog.

### Test plan

- Integration test invoking the binary with a fixture catalog.

### Out of scope

- `nav models add` / `nav providers add` (file as follow-up if desired).

---

## G8. `nav doctor` config diagnostics

**Labels**: `area/cli`, `weak-model-ok`

### Summary

Extend `nav doctor` to print:

1. Which providers are configured (built-in + user).
2. For each provider, whether its `api_key` resolves (G3) — show source
   (env / shell / literal / none), not the value.
3. The current `default_model`.
4. What `nav` would do if invoked right now: ChatGPT auth path or a specific
   provider path.

### Files

- `crates/nav-core/src/cli/commands/doctor.rs` (or wherever Doctor lives).

### Acceptance criteria

- [ ] Output includes a `## Providers` section.
- [ ] Credentials never leak into output — print `env:OPENAI_API_KEY (set)`
      or `shell command (resolves)` or `literal (length: 51)` or `not set`.
- [ ] A provider with an unresolvable credential is flagged as a warning,
      not an error (it may be intentionally unconfigured).
- [ ] If `default_model` points at an unresolvable provider, that's an error.

### Test plan

- Snapshot test of the doctor output against a curated fixture catalog.

### Out of scope

- A connectivity check that actually hits each provider's `/models` endpoint.
  Could be useful but adds network flake to `nav doctor`.

---

## G9. Auth auto-detect with config awareness

**Labels**: `area/config`, `area/model`, `weak-model-ok`

### Summary

Make the startup auth decision smarter. Current behavior bails when
`--auth chatgpt` (default) finds a missing `~/.codex/auth.json`. New
behavior: if Codex auth file is missing or in the wrong mode, fall through
to the resolved provider from G2.

### Background

`load_auth` in `crates/nav-core/src/model/auth.rs:39` hard-fails in two
common cases:

- `~/.codex/auth.json` doesn't exist (user never ran `codex login`).
- The file exists but `auth_mode` is not `chatgpt` (api-key Codex install).

In both cases, if the user has `default_model` configured (or set
`OPENAI_API_KEY`), nav should pick that path silently with a stderr nudge,
not fail hard.

### Decision tree

```
if --auth chatgpt (default) and codex auth file is valid:
    use Codex backend (today's behavior, untouched)
elif --auth chatgpt (default) and there's a resolvable default_model:
    print stderr nudge "Codex login missing; falling back to <provider/model>"
    use the resolved provider
elif --auth chatgpt and nothing resolvable:
    today's hard error (with the new note pointing at `nav providers list`)
elif --auth api-key:
    use the resolved provider from G2 (no fallback)
```

### Files

- `crates/nav-core/src/model/auth.rs:39` — extend `load_auth`.

### Acceptance criteria

- [ ] Codex auth path is unchanged when its file is valid.
- [ ] Fallback path is taken when Codex file is missing AND a resolvable
      provider exists.
- [ ] Fallback prints a single stderr line including which provider was
      picked.
- [ ] `--auth api-key` never falls back to Codex.
- [ ] Error message in the "nothing resolvable" case lists what was tried.

### Test plan

- Unit test: valid Codex file → Codex path.
- Unit test: missing Codex file + OPENAI_API_KEY → openai built-in path.
- Unit test: missing Codex file + no envs → hard error with the listing.

### Out of scope

- Migrating from `--auth` enum to a single `--provider` flag (would be
  cleaner long-term but is a breaking change).

---

## F1. Chat Completions wire format module

**Labels**: `area/model`, `needs-opus`

### Summary

Create `crates/nav-core/src/model/chat_completions/` as a parallel module to
`responses/`. This is the foundation issue — defines the module shape and
the wiring point from the agent loop into either backend. No request bodies
or parsing yet (those are C1/C2).

### Background

The existing Responses module
(`crates/nav-core/src/model/responses/mod.rs:174`) is tightly coupled to
the Codex/Responses wire format. Rather than refactor it behind a trait
(deferred until Responses gets wider adoption), we add a sibling module
with the same external shape and select between them at construction time.

### Module shape

```
crates/nav-core/src/model/chat_completions/
  mod.rs            // ChatCompletionsTransport, exports
  request.rs        // (C1) build_request_body
  parser.rs         // (C2) process_response, sanitize_continuation_items
  collector.rs      // accumulate streamed deltas into a ResponseEnvelope
  delta.rs          // (F2) SSE event normalizer
  sse.rs            // SSE connect + drive (no websocket)
  types.rs          // ChatCompletions-specific structs that map into shared types
  tests.rs
```

### Files

- `crates/nav-core/src/model/mod.rs:46` — `pub mod chat_completions;`.
- `crates/nav-core/src/model/chat_completions/mod.rs` — `ChatCompletionsTransport`
  implementing `ResponsesTransport`. (Keep the trait name even though it
  predates this work — renaming is a separate cleanup.)
- Transport selection: based on `ResolvedProvider` (G2). Codex auth →
  `OpenAiTransport` (existing). Provider catalog → `ChatCompletionsTransport`.

### Acceptance criteria

- [ ] New module compiles with stub `unimplemented!()` bodies in C1-style
      helpers (filled in by C1/C2/F2).
- [ ] `ChatCompletionsTransport::new(client, resolved, idle_timeout, retry_policy)`
      signature established.
- [ ] Agent loop selects the right transport given the resolved provider.
- [ ] No new code paths hit during Codex/ChatGPT runs — confirm via existing
      Codex test suite passing unchanged.

### Test plan

- Build-only sanity at this issue's scope.
- Existing test suite passes.

### Out of scope

- Actual request bodies (C1).
- Actual response parsing (C2).
- SSE event normalization (F2).

---

## F2. Chat Completions SSE event normalizer

**Labels**: `area/model`, `needs-opus`

### Summary

Translate streaming chunks from the Chat Completions SSE format into the
same internal event shape the Responses path produces, so downstream
event consumers (TUI, persistence, agent loop) don't need to know which
wire format is upstream.

### Background

Chat Completions SSE shape:

```
data: {"choices":[{"delta":{"content":"Hel"}}]}
data: {"choices":[{"delta":{"content":"lo"}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":""}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]}}]}
data: {"choices":[{"finish_reason":"tool_calls"}]}
data: [DONE]
```

Tool calls arrive as indexed deltas that the client must accumulate by
index — same shape OpenAI Chat Completions, Anthropic OpenAI shim, vLLM,
and OpenRouter all use. Note the `tool_calls` array can grow in length
mid-stream (parallel tool calls).

Responses path emits `response.output_text.delta`, `response.function_call_arguments.delta`,
`response.completed`. The normalizer needs to bridge these two vocabularies
so the existing `ResponseCollector`
(`crates/nav-core/src/model/responses/collector.rs`) and downstream event
plumbing keep working.

### Approach

Build a stateful `ChatCompletionsAccumulator` that:

1. Tracks current content buffer per choice (only choice 0 supported).
2. Tracks tool_calls by index, accumulating `function.name` and
   `function.arguments` (which arrives as a streamed JSON string).
3. Emits intermediate `AgentEvent::AssistantTextDelta` events.
4. On `finish_reason`, materializes a `ResponseEnvelope` shape that the
   existing collector can consume.

### Files

- `crates/nav-core/src/model/chat_completions/delta.rs` — accumulator.
- `crates/nav-core/src/model/chat_completions/sse.rs` — driver that calls
  the accumulator on each event.

### Acceptance criteria

- [ ] Pure-text streams produce the same `AssistantTextDelta` + final
      `Message` envelope as the Responses path.
- [ ] Tool-call streams produce equivalent `FunctionCall` items with
      correctly accumulated arguments (parseable JSON at finish).
- [ ] Parallel tool calls (multiple indices) materialize in the order
      `index 0, index 1, ...`.
- [ ] `[DONE]` terminates cleanly without producing a synthetic empty
      message.
- [ ] Mid-stream `error` events surface as `ResponsesError::Other` (or
      `ContextWindowExceeded` when applicable, see C4).
- [ ] Idle timeout behavior matches Responses path (reuse the existing
      idle-tracking pattern from `responses/sse.rs:74`).

### Test plan

- Fixture-driven snapshot tests against the SSE fixtures collected in C6.
- Unit test for parallel tool call accumulation.
- Unit test for mid-stream content + tool call interleaving.

### Out of scope

- Anthropic-OpenAI shim's slightly different `tool_use` shape (file
  follow-up if needed).
- Reasoning content streamed via `delta.reasoning_content` (DeepSeek-R1
  extension — file follow-up).

---

## F3. History conversion: persisted items → Chat Completions messages

**Labels**: `area/model`, `area/context`, `needs-opus`

### Summary

Pure function that converts nav's persisted, Responses-shaped continuation
items into the Chat Completions `messages` array.

### Background

nav stores turn history as Responses-shaped items
(`{type: "message"}`, `{type: "reasoning"}`, `{type: "function_call"}`,
`{type: "function_call_output"}`). When the resolved provider is a Chat
Completions endpoint, this needs translation:

| Persisted shape                                                    | Chat Completions shape                                                                              |
|--------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------|
| `{type: "message", role: "user", content: [{type:"input_text",...}]}` | `{role: "user", content: "..."}`                                                                  |
| `{type: "message", role: "assistant", content: [{type:"output_text",...}]}` | `{role: "assistant", content: "..."}`                                                       |
| `{type: "reasoning", ...}`                                         | **Dropped.** No equivalent in Chat Completions.                                                     |
| `{type: "function_call", call_id, name, arguments}`                | `{role: "assistant", tool_calls: [{id: call_id, type: "function", function: {name, arguments}}]}` |
| `{type: "function_call_output", call_id, output}`                  | `{role: "tool", tool_call_id: call_id, content: output}`                                            |

Plus: consecutive assistant text items get merged into one assistant message
(Chat Completions is unhappy with adjacent assistant turns).

### Files

- `crates/nav-core/src/model/chat_completions/history.rs` — new file with
  `pub fn responses_items_to_chat_messages(items: &[Value]) -> Vec<Value>`.

### Acceptance criteria

- [ ] Reasoning items dropped.
- [ ] `function_call` items collapse into the preceding assistant message
      when the assistant emitted both text and a tool call. If no preceding
      assistant text, emit a standalone `{role: "assistant", content: null,
      tool_calls: [...]}`.
- [ ] `function_call_output` items become `role: tool` messages with
      matching `tool_call_id`.
- [ ] System message (the nav instructions) is inserted as the first
      `role: "system"` message by the request builder (C1), not this
      converter.
- [ ] Empty content fields are normalized: assistant text that's empty
      becomes `null`, never `""`.
- [ ] Multi-part assistant content arrays (rare but legal) get concatenated.

### Test plan

- Snapshot tests for: pure user/assistant exchange, exchange with tool
  call, exchange with multiple parallel tool calls, exchange containing
  reasoning items (which must be dropped), exchange with empty assistant
  text.

### Out of scope

- Image content parts (file follow-up).
- Streaming this conversion (it's a small pure function run once per turn).

---

## F4. Live `/model` swap in TUI

**Labels**: `area/tui`, `area/model`, `needs-opus`

### Summary

Upgrade `/model` (G6) from "restart required" to "swap mid-session." When
the wire format changes (Codex→Chat Completions or vice versa), convert
the in-memory history via F3 on the fly.

### Background

Today's agent loop holds an `Arc<dyn ResponsesTransport>` constructed once
at startup (`crates/nav-core/src/agent_loop/runner.rs`). Live swap means
mutating the transport during a session. The harder part is history: if
we're switching from Codex (Responses-shaped) to a Chat Completions
provider, the next request body needs the converted shape.

### Approach

- Promote the transport from `Arc<dyn ResponsesTransport>` to an
  `Arc<Mutex<Box<dyn ResponsesTransport>>>` or a `RwLock`.
- Add a `swap_to(provider: ResolvedProvider)` method on whatever owns the
  transport.
- On a wire-format change, run F3 over the persisted history and replace
  the in-memory continuation buffer.
- Block the swap if a turn is currently mid-flight; queue it for after
  the current turn completes.

### Files

- `crates/nav-core/src/agent_loop/runner.rs` — transport ownership change.
- `crates/nav-core/src/agent_loop/mod.rs` — expose a swap entry point.
- `crates/nav-tui/src/input/commands.rs` — wire `/model` to the swap.

### Acceptance criteria

- [ ] Switching between two Chat Completions providers requires no history
      conversion (same wire format).
- [ ] Switching from Codex to Chat Completions runs F3 over the buffered
      continuation.
- [ ] Switching from Chat Completions back to Codex is **rejected** with
      a clear message — reverse conversion (synthesizing reasoning items)
      isn't worth supporting yet.
- [ ] Swap during an in-flight turn is queued, not raced.
- [ ] Tested via a TUI-level test that submits a turn, swaps, submits
      another turn.

### Test plan

- Integration test with stub transports for each side.
- Manual TUI smoke test.

### Out of scope

- Persisting the new model choice across restarts (G6 already covers that).
- Swapping reasoning effort live (could be added later; needs only request-
  builder awareness).

---

## C1. Chat Completions request body builder

**Labels**: `area/model`, `weak-model-ok`

### Summary

Pure function that builds a Chat Completions request body from nav's input
items, tool definitions, and instructions.

### Background

Mirror of `crates/nav-core/src/model/responses/request.rs:58` but for the
Chat Completions shape:

```json
{
  "model": "glm-5.1",
  "messages": [
    {"role": "system", "content": "<nav instructions>"},
    {"role": "user", "content": "..."}
  ],
  "tools": [
    {"type": "function", "function": {"name": "...", "description": "...", "parameters": {...}}}
  ],
  "stream": true,
  "reasoning_effort": "high"
}
```

History items come through F3. Tool wrapping comes through C3. This issue
glues those together plus the top-level fields.

### Files

- `crates/nav-core/src/model/chat_completions/request.rs` — new.

### Acceptance criteria

- [ ] `stream: true` always set (we're an interactive client).
- [ ] System message built from nav's instructions (
      `crate::context::build_instructions`) is the first message.
- [ ] `messages` array comes from F3.
- [ ] `tools` comes from `tool_definitions` (existing function) wrapped via
      C3.
- [ ] `reasoning_effort` included only when `Some`.
- [ ] `max_tokens` set from `ResolvedProvider.max_output_tokens` when set.
- [ ] No Responses-only knobs: no `prompt_cache_key`, no `store`, no
      `include: ["reasoning.encrypted_content"]`.

### Test plan

- Snapshot tests of the JSON for: simple user turn, turn with a tool call
  in history, turn with `reasoning_effort: "high"`, turn with `max_tokens`.

### Out of scope

- Provider-specific extras like OpenRouter routing (`provider:` key).
- Anthropic shim's prompt caching headers.

---

## C2. Chat Completions response envelope parser

**Labels**: `area/model`, `weak-model-ok`

### Summary

Translate accumulated Chat Completions response state into the existing
`ResponseEnvelope` and `TurnUsage` shapes so the rest of the agent loop
doesn't need to branch on wire format.

### Background

Existing parser:
`crates/nav-core/src/model/responses/parser.rs:14` returns `Vec<ToolCall>`
from a Responses envelope. C2 builds the equivalent path from the
accumulator output (F2). Usage mapping:

| Chat Completions field                                | TurnUsage field      |
|-------------------------------------------------------|----------------------|
| `usage.prompt_tokens`                                 | `tokens_input`       |
| `usage.completion_tokens`                             | `tokens_output`      |
| `usage.prompt_tokens_details.cached_tokens`           | `tokens_input_cached`|
| (no equivalent)                                       | `tokens_reasoning` = 0 |

### Files

- `crates/nav-core/src/model/chat_completions/parser.rs` — new.

### Acceptance criteria

- [ ] `process_response(response: &ResponseEnvelope) -> Result<Vec<ToolCall>>`
      reuses the existing `ToolCall` type.
- [ ] `assistant_text` returns the assistant content string.
- [ ] `sanitize_continuation_items` for Chat Completions: nothing to
      sanitize (no encrypted reasoning), but provide a no-op or
      identity-with-filter so the call site shape stays consistent.
- [ ] `turn_usage_from` populates the four `TurnUsage` fields per the table
      above.
- [ ] Missing `usage` block parses as `TurnUsage::default()`.

### Test plan

- Unit tests against fixed envelope fixtures.

### Out of scope

- Multi-choice responses (`n > 1` — we always send `n: 1`).

---

## C3. Tool definition wire-shape wrapper

**Labels**: `area/model`, `weak-model-ok`

### Summary

Wrap existing tool definitions for Chat Completions. Responses uses
`{type: "function", name, description, parameters}` flat at the top level;
Chat Completions requires `{type: "function", function: {name, description,
parameters}}`. Plus tool calls themselves get a slightly different shape
(`type: "function"` rather than `function_call`), which lives in C2.

### Files

- `crates/nav-core/src/model/chat_completions/request.rs` — small helper
  `wrap_tools(tools: &[Value]) -> Vec<Value>`.

### Acceptance criteria

- [ ] Each tool definition's `{name, description, parameters}` is moved
      under a `function` key.
- [ ] `type: "function"` set at the top level.
- [ ] Order preserved.
- [ ] Pure function, no allocations beyond what's necessary.

### Test plan

- Unit test wrapping a few representative tool definitions.

### Out of scope

- The `strict: true` field some providers accept (file follow-up).

---

## C4. Context-overflow detection for Chat Completions

**Labels**: `area/model`, `weak-model-ok`

### Summary

Detect "you exceeded the context window" responses from Chat Completions
providers and surface them as `ResponsesError::ContextWindowExceeded` so the
existing agent-loop recovery path (drop oldest tool pair, retry) works
unchanged.

### Background

`crates/nav-core/src/model/responses/mod.rs:147` does this for Responses.
The Chat Completions shape:

```json
{"error": {"message": "...", "type": "invalid_request_error", "code": "context_length_exceeded"}}
```

But codes vary by provider:
- OpenAI: `context_length_exceeded`
- DeepSeek: same.
- Anthropic OpenAI shim: `invalid_request_error` with the substring
  `max_tokens` or `input is too long`.
- Together / Groq: usually `context_length_exceeded`, sometimes the
  message-string variant.

### Files

- `crates/nav-core/src/model/chat_completions/mod.rs` — sibling of the
  existing `detect_http_overflow` and `detect_context_overflow`.

### Acceptance criteria

- [ ] HTTP-status path: parse error body, return `Some(message)` when
      `error.code == "context_length_exceeded"` or
      `error.message` contains `"maximum context length"` /
      `"context length"`.
- [ ] Stream-event path: parse SSE `error` events with the same logic.
- [ ] Conservative: when in doubt, return `None` and let it surface as a
      regular error rather than triggering recovery on a false positive.

### Test plan

- Fixture-driven tests covering at least OpenAI and one other provider.

### Out of scope

- Anthropic shim's wholly different error format (file follow-up).

---

## C5. Quiet the model-name typo guard for non-OpenAI providers

**Labels**: `area/model`, `weak-model-ok`

### Summary

The typo guard in `crates/nav-core/src/model/names.rs:8` and the "Did you
mean…?" enrichment in `crates/nav-core/src/model/responses/mod.rs:104` are
OpenAI-specific. When the resolved provider isn't OpenAI, skip both —
otherwise users on `glm-5.1` get unhelpful suggestions like "Did you mean
gpt-5?".

### Files

- `crates/nav-core/src/startup_notices.rs` — gate the typo nudge on whether
  the resolved provider is the OpenAI built-in (or any provider whose
  `base_url` starts with `https://api.openai.com`).
- `crates/nav-core/src/model/chat_completions/mod.rs` — its own
  `model_hint_from_body` that returns `None` always (or is just absent and
  the call site skips it).

### Acceptance criteria

- [ ] Running with a non-OpenAI provider produces no "Did you mean…?" output.
- [ ] Running with the OpenAI built-in still gets the existing nudge.
- [ ] Detection done by provider id, not by sniffing the model string.

### Test plan

- Unit test that the gate's predicate returns the right thing for each
  built-in.

### Out of scope

- A per-provider suggestion pool (would need each provider's model list
  curated and kept current — too much maintenance for marginal value).

---

## C6. Recorded Chat Completions SSE fixtures

**Labels**: `area/test`, `weak-model-ok`

### Summary

Record real SSE streams from one OSS Chat Completions provider (Ollama or
vLLM is easiest — local, free, no API key) and save them as fixtures for
F2's snapshot tests.

### Background

F2's accumulator design is taste-heavy and easy to get subtly wrong
(tool-call argument ordering, finish_reason placement, `[DONE]` framing).
Real fixtures lock the behavior against an actual server.

### Files

- `crates/nav-core/tests/fixtures/chat_completions_sse/` — new directory.
  - `text_only.sse`
  - `single_tool_call.sse`
  - `parallel_tool_calls.sse`
  - `text_then_tool_call.sse`
  - `context_overflow_error.sse`

### Acceptance criteria

- [ ] At least 5 fixtures covering the scenarios above.
- [ ] Each fixture has a sidecar `.json` describing what should come out
      of the accumulator (assistant text, tool calls in order, finish
      reason, usage).
- [ ] Recording script committed at
      `crates/nav-core/tests/fixtures/chat_completions_sse/record.sh` so
      anyone can refresh fixtures against a running Ollama / vLLM.

### Test plan

- F2 consumes these fixtures (cross-issue dependency).

### Out of scope

- Fixtures from cloud providers (would require an API key in CI).

---

## D1. Provider cookbook in README

**Labels**: `area/docs`, `weak-model-ok`

### Summary

One copy-pasteable section per built-in provider in `README.md`, showing the
minimum settings.json snippet and the env var to export.

### Files

- `README.md` — new `## Providers` section.

### Each entry

```
### OpenRouter

  export OPENROUTER_API_KEY=sk-or-...

  // ~/.nav/settings.json
  {
    "providers": {
      "openrouter": {
        "models": {
          "qwen3.7-coder": { "model_id": "qwen/qwen3.7-coder" }
        }
      }
    },
    "default_model": "openrouter/qwen3.7-coder"
  }

  nav "fix the off-by-one in compaction.rs"
```

### Acceptance criteria

- [ ] One entry per built-in in G4.
- [ ] Each entry shows: env var, minimal settings.json snippet, invocation
      example.
- [ ] One entry for "macOS Keychain" showing the `!security find-generic-password`
      form via G3.
- [ ] One entry for a custom self-hosted vLLM showing the full provider
      object (since it's not a built-in).

### Out of scope

- A full feature comparison table across providers.

---

## D2. Auth migration note

**Labels**: `area/docs`, `weak-model-ok`

### Summary

Short README section explaining the relationship between the existing
`--auth chatgpt` flow and the new provider catalog, so existing users
aren't confused about which one to use.

### Content

- ChatGPT subscription users: nothing changes. `--auth chatgpt` (the
  default) still picks up `~/.codex/auth.json`.
- Users who want any other model: set `OPENAI_API_KEY` (or another
  provider's env var), and either pass `--model openai/gpt-5.5` or set
  `default_model` in settings.json.
- The Codex path uses the Responses API; everything else uses Chat
  Completions. End users normally don't need to think about this, but
  document it for power users.
- `/model` switches between configured models; switching from Codex to a
  Chat Completions provider mid-session is supported, the reverse isn't.

### Files

- `README.md` — append after D1's `## Providers` section.

### Acceptance criteria

- [ ] Under 400 words.
- [ ] No ambiguity about which path is the default.
- [ ] Mentions the live-swap restriction (F4's reverse-direction veto).

### Out of scope

- Migration of session storage formats.
- Anything about future Responses-API plans.
