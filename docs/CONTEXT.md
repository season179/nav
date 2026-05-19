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
                    │  --json-rpc: protocol on stdout  │
                    └──────────────┬───────────────────┘
                                   │
       ┌───────────────────────────┼───────────────────────────┐
       │                           │                           │
  terminal user              nav-desktop                  Cloudflare
                             (Electron)                   Worker
                             spawns `nav                  spawns sandbox,
                             --json-rpc`,                 runs `gh clone` +
                             parses JSON-RPC              `nav --json-rpc`,
                                                          relays to chat
```

Two seams matter:

- **`AgentEvent` (in-process)** — what `nav-core` emits. `nav-cli` consumes
  it directly. Any future Rust frontend would too.
- **Headless JSON-RPC on stdout** — the stable non-TUI wire format. It wraps
  `AgentEvent` payloads in versioned `nav.event` notifications and exposes
  approval responses through stdin. `--json-events` remains as a raw debugging
  stream.

The desktop UI requires a working directory before it can run the agent. The
chat-bot frontend gets its working directory by cloning the requested repo
inside a fresh Cloudflare Sandbox container; that container is the session's
isolation boundary, and credentials (API key, GitHub token) are injected by
the Worker per-session. The Worker is also responsible for resolving the
project reference (e.g. "project Y") to an `owner/repo` before invoking nav,
so `nav-core` itself stays generic.

There is no long-lived `nav-server` daemon. If long-lived multi-session state
later becomes necessary, it can be added without rewriting any of the above.

## Skills

`nav` implements the [agentskills.io](https://agentskills.io) client protocol:
agent skills are directories containing a `SKILL.md` file with YAML
frontmatter (`name`, `description`). Discovery happens once when `nav`
starts, scoped to the process launch cwd:

1. Read `<launch_cwd>/.agents/skills/` — these are project skills. We do not
   walk upward to ancestor directories.
2. Read `~/.agents/skills/` — these are user skills.
3. Project entries shadow user entries with the same parsed `name` and the
   shadowing is logged with both paths.

The discovered [`Catalog`] is shared by three seams:

- `response_body` injects a compact "Available skills" list into the system
  prompt — `name`, `description`, absolute `SKILL.md` path, and `skill_dir`.
  The model loads instructions on demand using its existing file-read tool
  rather than via any dedicated `activate_skill` tool.
- `run_tool` accepts paths under any catalog `skill_dir` as additional read
  roots for `read_file`, `list_files`, and `code_search`, so the absolute
  paths the system prompt advertises are actually resolvable. Writes
  (`edit_file`) remain workspace-only.
- The TUI slash popup is sourced from the same catalog: built-in commands
  (`/help`, `/clear`, `/quit`, `/resume`, `/sessions`) plus one entry per
  skill. Submitting `/<skill-name> <request>` wraps the SKILL.md body in a
  `<skill name=… dir=…>` block and inlines `<request>` for the next turn.
  Submitting `/<skill-name>` alone queues the wrapped body in TUI state and
  prepends it to the next non-slash prompt; each `run_agent` call is
  independent of prior turns, so without this queue the activation would
  not reach the model alongside the actual request.

Skills that are malformed (missing description, unparseable frontmatter) are
skipped with a diagnostic. Cosmetic issues like a `name`/directory mismatch
warn but still load.

## Extensions

`nav` also has a small local extension manifest format. Discovery is scoped to
the launch cwd, just like settings and skills:

1. Read `<launch_cwd>/.nav/extensions/*/extension.json`.
2. Read `~/.nav/extensions/*/extension.json`.
3. Project prompt templates and themes shadow user entries with the same
   registered `name`.

The shipped runtime surface is intentionally narrow:

- `prompt_templates` register markdown files that appear in the TUI slash
  popup as `/prompt:<name>`. Submitting `/prompt:<name> <request>` wraps the
  template body in a `<prompt_template ...>` block and sends it with the
  visible request; submitting `/prompt:<name>` queues it for the next prompt.
- `themes` register simple TUI color overrides. `.nav/settings.json` can set
  `"theme": "<name>"`; unknown or invalid colors fall back to the built-in
  dark theme.
- `nav extensions list` prints discovered manifests and counts future-facing
  sections.

Manifest sections for `custom_tools`, `mcp_servers`, `hooks`, and `packages`
are parsed only as counts for now. They are not executed; that keeps the first
extension slice useful without adding an unaudited local command surface.

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
3. Add a `--json-events` flag to `nav-cli` that emits `AgentEvent` as raw
   NDJSON on stdout, then stabilize the frontend contract as `--json-rpc`.
4. Rewire `nav-desktop` from "spawn `cargo run`, line-parse stdout" to
   "spawn `nav --json-rpc`, parse versioned notifications." Drop the current
   string matching in the renderer.
5. Build the first chat-bot frontend: a Cloudflare Worker that receives
   Slack events, spawns a Cloudflare Sandbox per session, clones the repo
   via `gh`, runs `nav --json-rpc`, and relays events into the Slack
   thread. Auth is API-key, injected by the Worker.
6. Keep tests focused on safety boundaries, transcript correctness, and tool
   execution behavior.
