# nav

`nav` is a learning project: a Rust coding-agent harness built to understand
how coding agents actually work by building one.

The first goal is learning. `nav` should keep the agent loop, tool plumbing,
transport, approvals, session replay, compaction, and frontend contracts close
enough to the surface that they can be studied and changed without digging
through a giant framework.

The second goal is personal usefulness. As the harness gets clearer, it should
also become good enough for real work in this checkout and nearby projects: safe
around the workspace, explicit about what it is doing, resumable after longer
sessions, and pleasant enough to use from the terminal.

## Current Focus

- **Learn the harness:** keep the model loop, tool calls, event stream,
  approval flow, replay, and compaction behavior understandable from the code.
- **Make it useful for me:** inspect files, edit code, run commands, show
  diffs, ask for approval when needed, and keep a durable local session log.
- **Understand long sessions:** support manual and automatic compaction, then
  improve normal pre-compaction turns so old tool output does not bloat context
  or disappear in a confusing way.
- **Expose clean frontend seams:** keep one Rust agent loop and expose it
  through the TUI, raw NDJSON, JSON-RPC, and future experiments.
- **Stay local and auditable:** use local SQLite for history, default to
  ChatGPT OAuth or an explicit API key, restrict file writes to the workspace,
  and make shell approval/sandbox behavior visible.

For the broader project context, read [docs/CONTEXT.md](docs/CONTEXT.md). For a
guided code tour, open [docs/ARCHITECTURE.html](docs/ARCHITECTURE.html).

## What Works Today

- A terminal TUI when you run `nav` in a real terminal.
- Headless raw events with `--json-events`.
- Versioned JSON-RPC notifications with `--json-rpc` for non-TUI experiments.
- Session persistence, resume, export, fork, labels, transcript search, and
  session trees.
- Workspace-aware tools: read files, list files, search with ripgrep, run
  shell commands, edit files, apply patches, and spawn read-only helper agents.
- Approval prompts, dangerous-command blocking, protected-file checks, and a
  macOS sandbox for shell commands.
- Manual `/compact`, automatic pre-turn compaction, context reports, and
  bounded tool output.

Learning threads still in progress:

- Budgeted replay of prior tool calls/results before compaction.
- Native `read_file` slicing with `offset` and `limit`.
- Observability that helps explain real runs: tokens, tools, retries,
  approvals, compactions, and failures.
- Frontend experiments beyond the terminal when they teach something
  useful about harness boundaries.

## Requirements

- Rust toolchain with Cargo.
- `rg` from ripgrep on `PATH`; `code_search` shells out to it.
- Codex login for the default ChatGPT OAuth flow, or `OPENAI_API_KEY` for raw
  API-key mode.

## Setup

Sign in once through Codex so `~/.codex/auth.json` exists:

```sh
codex login
```

Run from source:

```sh
cargo run -- "List the files and explain this project"
```

Install the `nav` binary from this checkout:

```sh
cargo install --path crates/nav-cli
nav "List the files and explain this project"
```

By default `nav` uses:

- model: `gpt-5.5`
- auth: ChatGPT OAuth from `~/.codex/auth.json`
- transport: WebSocket
- approval policy: `on-request`
- sandbox: `workspace-write`

Use an API key instead:

```sh
OPENAI_API_KEY=... nav --auth api-key "Run the test suite"
```

Run a quick local health check:

```sh
nav doctor
nav doctor --json
```

## Terminal TUI

Run `nav` with no prompt in a terminal:

```sh
cd ~/code/my-project
nav
```

The TUI shows the transcript above and a multi-line composer below.

Useful keys and commands:

- `Enter` submits. `Shift+Enter` inserts a newline.
- `Ctrl+U` clears to the start of the line.
- `Ctrl+W` deletes the previous word.
- `Up` and `Down` recall earlier prompts from the same session.
- `/help` shows slash commands.
- `/clear` clears the visible transcript.
- `/context` estimates what the next model request will contain.
- `/context all` expands the context report into item-level rows.
- `/compact` summarizes older history and continues from the checkpoint.
- `/sessions` lists stored sessions.
- `/resume` opens the session picker.
- `/quit` exits. Press `Ctrl+C` twice to exit cleanly.

Prompt templates and skills also appear in the slash popup when discovered.

## Headless Modes

Use raw NDJSON when you want one `AgentEvent` per line:

```sh
nav --json-events "list the files" > events.ndjson
```

Use JSON-RPC for a script, chat bridge, or another frontend experiment:

```sh
nav --json-rpc "list the files"
```

JSON-RPC mode emits newline-delimited JSON-RPC 2.0 notifications:

- `nav.session.started` announces `protocol_version`, session id, cwd, model,
  and transport.
- `nav.event` wraps the same `AgentEvent` payloads the TUI consumes.
- `nav.approval.respond` can be written to stdin to answer approval requests.

If stdout is not a TTY, `nav` uses raw headless events automatically unless
you pass `--json-rpc`.

## Sessions

Every run is stored in SQLite at `$XDG_DATA_HOME/nav/nav.db`, falling back to
`~/.local/share/nav/nav.db`. Use `--db-path` to override it. Relative database
paths are resolved inside the nav data directory.

Common session commands:

```sh
nav --list-sessions
nav --list-sessions --cwd "$PWD"
nav --resume <session-id> "Continue from here"
nav --pick-session
nav export <session-id> --format md --out transcript.md
```

Advanced session workflows:

```sh
nav sessions fork <session-id> --name "try another approach"
nav sessions tree <session-id>
nav sessions label <session-id> bugfix
nav sessions search "panic" --label bugfix
```

Token rollups appear in session listings. Cost is shown only when a provider
reports it.

## Safety Model

`nav` is meant to be useful without being casual about your filesystem.

- `read_file`, `list_files`, `edit_file`, `apply_patch`, and `code_search`
  reject absolute workspace paths, `..`, and symlink escapes.
- Writes are workspace-only.
- Reads of `.env*`, `*.pem`, `*.key`, and SSH keys require approval.
- Writes under `.git`, `.agents`, and `.nav` are blocked.
- `bash` defaults to `--sandbox workspace-write`.
- On macOS, shell commands run through `sandbox-exec`.
- On Linux and Windows, sandboxing is currently passthrough; command
  classification and protected-path rules still apply.
- Dangerous commands can require approval or be refused outright.

The main knobs are:

```sh
nav --approval-policy untrusted "inspect this repo"
nav --approval-policy never --json-events "run read-only checks"
nav --sandbox read-only "look around without writing"
nav --sandbox danger-full-access "I know this needs full access"
```

`--dangerously-bypass-approvals-and-sandbox` bypasses prompts and sandboxing,
but unbypassable dangerous commands and protected-metadata writes are still
refused.

## Tools

The model can call these tools:

- `read_file`: read a relative file path.
- `list_files`: list a relative directory.
- `code_search`: search with ripgrep.
- `bash`: run a shell command with timeout, approval, and sandbox handling.
- `edit_file`: create a file or replace one exact string.
- `apply_patch`: apply a reviewable multi-file patch.
- `spawn_subagent`: run a focused read-only helper agent for exploration or
  review.

Large tool output is bounded before it goes back to the model. Shell output can
spill the full raw result to local storage while the model sees a smaller
head/tail view.

## Git Checkpoints

For reversible work, `nav` can create stash-backed checkpoints:

```sh
nav git checkpoint "before refactor"
nav git stash "pause this work"
nav git list
nav git restore
```

Enable automatic dirty-worktree checkpoints before normal turns:

```sh
nav --git-checkpoints "make the change"
```

Or put `"git_checkpoints": true` in settings.

## Project Context And Settings

At startup, `nav` reads context files only from the launch directory:

- `<cwd>/AGENTS.md`
- `<cwd>/CLAUDE.md`
- `~/.agents/AGENTS.md`
- `~/.agents/CLAUDE.md`

It does not walk upward through parent directories.

Settings can live in `<cwd>/.nav/settings.json` or `~/.nav/settings.json`.
Project settings override user settings. Explicit CLI flags override both.

Example:

```json
{
  "model": "gpt-5.5",
  "transport": "websocket",
  "bash_timeout_secs": 30,
  "auto_compact_token_limit": 200000,
  "auto_compact_fraction": 1.0,
  "ambient_context_token_budget": 256,
  "git_checkpoints": true,
  "theme": "night"
}
```

Malformed settings fall back to defaults with an error on stderr. Unknown keys
are rejected so typos do not silently change behavior.

## Skills And Extensions

Project skills live in `<cwd>/.agents/skills/`. User skills live in
`~/.agents/skills/`. Project skills shadow user skills with the same parsed
name. Skills are readable by the agent but not writable by tool calls.

Local extensions live in:

- `<cwd>/.nav/extensions/<name>/extension.json`
- `~/.nav/extensions/<name>/extension.json`

Today, extensions can register:

- prompt templates, available as `/prompt:<name>` in the TUI
- simple TUI theme color overrides

`nav extensions list` shows discovered manifests. Manifest sections for custom
tools, MCP servers, hooks, and packages are parsed for visibility but are not
executed yet.

## Active Design Notes

- [docs/context-management-plan.md](docs/context-management-plan.md)
  ranks the next token-efficiency work: lazy skills section, prompt caching,
  proactive pruning, budgeted tool-call replay with placeholders, and sliced
  `read_file` reads, plus Tier 2 UX (edit/restore, handoff, `@file`).
- [docs/otel-observability-prd.md](docs/otel-observability-prd.md) explains
  the proposed telemetry shape: local logs stay canonical, while optional OTLP
  traces make usage, failures, approvals, retries, compactions, and token
  pressure easier to study.
- [docs/TODO.md](docs/TODO.md) tracks shipped daily-driver work and remaining
  follow-ups.

## Development Checks

Useful local checks:

```sh
cargo fmt --all -- --check
cargo test -p nav-core -p nav-cli -p nav-tui
cargo clippy --workspace --all-targets -- -D warnings
```

Snapshot tests use `insta`. Review pending snapshots before committing.
