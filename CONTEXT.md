# nav Context

`nav` began as an educational Rust implementation of a coding-agent loop. That
purpose still matters: the code should remain readable, direct, and useful for
learning how a Responses-based agent works.

From this point onward, `nav` also has a product goal: become a genuinely usable
local coding agent for real development work.

## Product Direction

`nav` should grow from a CLI demo into a dependable coding assistant that can:

- inspect a workspace and explain what it finds;
- edit files with clear boundaries and reviewable changes;
- run commands and tests with visible output;
- stream model messages, tool calls, and tool results as first-class events;
- preserve enough session state to make longer tasks understandable;
- expose a UI suitable for daily use on macOS and Windows.

The UI direction should assume a local agent backend rather than putting all
agent logic in the frontend. The likely shape is:

```text
Rust agent core -> local event/API layer -> desktop UI
```

For a desktop UI, prefer options that preserve the Rust core and support both
macOS and Windows. As of the current research pass, Tauri v2 is the leading
candidate, with Electron as the pragmatic fallback if IDE-like UI requirements
outgrow system WebViews.

## Engineering Principles

- Keep the educational path clear. A new contributor should still be able to
  read the main loop and understand the agent.
- Do not hide agent behavior behind magic abstractions. Tool calls, transcript
  state, auth, transport, and filesystem boundaries should stay explicit.
- Productize the seams that matter: streaming events, tool execution records,
  command output, file edits, diffs, errors, and session history.
- Treat workspace safety as a product requirement, not a demo detail. Path
  restrictions, shell execution, and future approval flows should remain easy to
  audit.
- Prefer small vertical slices over broad rewrites. When adding UI/backend
  features, keep the CLI working unless there is a deliberate migration plan.

## Near-Term Priorities

1. Split the agent loop from the CLI entrypoint so it can be driven by both the
   terminal and a UI backend.
2. Emit structured events for assistant messages, tool calls, tool results,
   command output, errors, and completion.
3. Add a local API layer for a desktop UI to start sessions and subscribe to
   events.
4. Design the first UI around the real coding workflow: transcript, tool log,
   command output, file/diff view, and workspace controls.
5. Keep tests focused on safety boundaries, transcript correctness, and tool
   execution behavior.
