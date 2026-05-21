# nav Context

`nav` is first a learning project: a Rust coding-agent harness for understanding
how coding agents work by building one. The code should remain readable,
direct, and useful for studying Responses-based agent loops, tool plumbing,
approval flow, replay, compaction, and frontend boundaries.

`nav` also has a secondary product goal: become useful enough for personal local
development work without making the harness harder to understand.

## Product Direction

`nav` should grow from a CLI demo into a clear, dependable personal coding
assistant that can:

- inspect a workspace and explain what it finds;
- edit files with clear boundaries and reviewable changes;
- run commands and tests with visible output;
- stream model messages, tool calls, and tool results as first-class events;
- preserve enough session state to make longer tasks understandable;
- expose a stable headless protocol for scripts, chat bridges, and other
  experiments when they teach something useful about harness boundaries.

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
       ┌───────────────────────────┴───────────────────────────┐
       │                                                       │
  terminal user                                      future experiments
                                                     scripts, chat bridges,
                                                     or sandboxed runners
                                                     using `nav --json-rpc`
```

Two seams matter:

- **`AgentEvent` (in-process)** — what `nav-core` emits. `nav-cli` consumes
  it directly. Any future Rust frontend would too.
- **Headless JSON-RPC on stdout** — the stable non-TUI wire format. It wraps
  `AgentEvent` payloads in versioned `nav.event` notifications and exposes
  approval responses through stdin. `--json-events` remains as a raw debugging
  stream.

Future non-TUI frontends should get their working directory explicitly before
invoking `nav --json-rpc`. A chat-bot experiment, for example, could clone a
requested repo inside a fresh sandbox container and use that container as the
session's isolation boundary. That shape should stay outside `nav-core` unless
the harness needs a smaller shared seam.

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
  (including `/help`, `/context`, `/compact`, `/handoff`, and session
  management commands) plus one entry per skill. Submitting
  `/<skill-name> <request>` wraps the SKILL.md body in a
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
- Productize only the seams that make the harness clearer or more useful:
  streaming events, tool execution records, command output, file edits, diffs,
  errors, and session history.
- Treat workspace safety as a learning and usability requirement, not a demo
  detail. Path restrictions, shell execution, and approval flows should remain
  easy to audit.
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
4. Keep `--json-rpc` as the clean headless contract for future frontend or
   sandbox experiments, without keeping unused frontend code around before it is
   useful.
5. Keep tests focused on safety boundaries, transcript correctness, and tool
   execution behavior.
