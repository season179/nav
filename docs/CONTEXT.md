# nav Context

`nav` began as an educational Rust implementation of a coding-agent loop. That
purpose still matters: the code should remain readable, direct, and useful for
learning how a Responses-based agent works.

From this point onward, `nav` also has a product goal: become a genuinely usable
coding agent for real development work.

## Product Direction

`nav` should grow from a CLI demo into a dependable coding assistant that can:

- inspect a workspace and explain what it finds;
- edit files with clear boundaries and reviewable changes;
- run commands and tests with visible output;
- stream model messages, tool calls, and tool results as first-class events;
- preserve enough session state to make longer tasks understandable;
- be reachable from multiple surfaces: terminal, desktop, and chat
  (Slack / Discord / Telegram).

## Architecture

The agent loop is a Rust library. Every surface is a thin frontend that runs
the same binary and reads the same event stream:

```
                    ┌──────────────────────────────────┐
                    │  nav-core  (library)             │
                    │  agent loop, tools, transport    │
                    │  emits AgentEvent                │
                    └──────────────┬───────────────────┘
                                   │
                    ┌──────────────┴───────────────────┐
                    │  nav-cli  (binary, named `nav`)  │
                    │  default: terminal output        │
                    │  --json-events: NDJSON on stdout │
                    └──────────────┬───────────────────┘
                                   │
       ┌───────────────────────────┼───────────────────────────┐
       │                           │                           │
  terminal user              nav-desktop                  Cloudflare
                             (Electron)                   Worker
                             spawns `nav                  spawns sandbox,
                             --json-events`,              runs `gh clone` +
                             parses NDJSON                `nav --json-events`,
                                                          relays to chat
```

Two seams matter:

- **`AgentEvent` (in-process)** — what `nav-core` emits. `nav-cli` consumes
  it directly. Any future Rust frontend would too.
- **NDJSON `AgentEvent` on stdout** — the wire format. `nav-desktop` and the
  chat-bot Worker both consume this. Same protocol, two adapters.

The desktop UI requires a working directory before it can run the agent. The
chat-bot frontend gets its working directory by cloning the requested repo
inside a fresh Cloudflare Sandbox container; that container is the session's
isolation boundary, and credentials (API key, GitHub token) are injected by
the Worker per-session. The Worker is also responsible for resolving the
project reference (e.g. "project Y") to an `owner/repo` before invoking nav,
so `nav-core` itself stays generic.

There is no long-lived `nav-server` daemon. If long-lived multi-session state
later becomes necessary, it can be added without rewriting any of the above.

## Engineering Principles

- Keep the educational path clear. A new contributor should still be able to
  read the main loop and understand the agent.
- Do not hide agent behavior behind magic abstractions. Tool calls, transcript
  state, auth, transport, and filesystem boundaries should stay explicit.
- Productize the seams that matter: streaming events, tool execution records,
  command output, file edits, diffs, errors, and session history.
- Treat workspace safety as a product requirement, not a demo detail. Path
  restrictions, shell execution, and future approval flows should remain easy
  to audit. The Cloudflare Sandbox path strengthens this by making each
  chat-bot session run in its own ephemeral container.
- Prefer small vertical slices over broad rewrites. When adding frontends or
  features, keep the CLI working unless there is a deliberate migration plan.

## Near-Term Priorities

1. Promote the repo to a Cargo workspace: `nav-core` (library) and `nav-cli`
   (binary named `nav`). The current agent code in `src/` moves into
   `nav-core` so the same loop drives every frontend.
2. Extract the agent loop from `nav-cli`'s `main` into a `run_agent` function
   in `nav-core`. Define `AgentEvent` (assistant message, tool call, tool
   result, command output, error, turn complete).
3. Add a `--json-events` flag to `nav-cli` that emits `AgentEvent` as NDJSON
   on stdout. This becomes the wire format for every non-Rust frontend.
4. Rewire `nav-desktop` from "spawn `cargo run`, line-parse stdout" to
   "spawn `nav --json-events`, parse NDJSON." Drop the current string
   matching in the renderer.
5. Build the first chat-bot frontend: a Cloudflare Worker that receives
   Slack events, spawns a Cloudflare Sandbox per session, clones the repo
   via `gh`, runs `nav --json-events`, and relays events into the Slack
   thread. Auth is API-key, injected by the Worker.
6. Keep tests focused on safety boundaries, transcript correctness, and tool
   execution behavior.
