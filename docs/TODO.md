# TODO

## Provider API Adapters

- [ ] Keep nav local-first. Do not depend on provider-side stored conversation
  state by default.
- [ ] Add a provider adapter boundary so `nav-core` works with nav's own
  normalized messages, tool calls, tool results, usage, and errors.
- [ ] Keep OpenAI-specific details inside the OpenAI adapter: Responses API
  input shape, `store: false`, encrypted reasoning content, and
  `function_call` / `function_call_output` items.
- [ ] Define how Anthropic-style APIs map into the same internal shape:
  content blocks, `tool_use`, `tool_result`, and continuation state.
- [ ] Define how completion-style APIs behave when they do not have native tool
  calling, either as a reduced mode or through a structured-output wrapper.
- [ ] Persist only provider-neutral conversation state locally by default. If a
  provider needs opaque continuation data, store it locally with a provider name
  and version label.
- [ ] Add recorded-fixture tests for each provider adapter before exposing it in
  the TUI.
