# nav

`nav` is a small Rust coding agent built on the Responses API.

This keeps the spirit of Geoffrey Huntley's workshop: a coding agent is a loop
that can read, edit, search, run commands, and report back. The project started
as an educational implementation, but now also has a second goal: become a
usable local coding agent with a real product surface. Keep the simple learning
path intact, but prefer designs that can grow into reliable day-to-day use.

See [CONTEXT.md](CONTEXT.md) for the current product direction and engineering
priorities. If you want to read the code, start with the guided tour in
[ARCHITECTURE.html](ARCHITECTURE.html) — it walks through `nav-core` and
`nav-tui` in plain English and tells you which files to open in which order.

By default it uses the ChatGPT OAuth credentials stored by Codex in
`~/.codex/auth.json` and calls the Codex Responses backend directly over
WebSocket.

WebSocket mode keeps each turn on one low-latency transport. The SSE
transport is still available with `--transport sse` because plain HTTP is useful
for learning the shape of streamed Responses events.

## Prerequisites

- `rg` from ripgrep must be on `PATH` for the `code_search` tool.

## Setup

```sh
cargo run -- "Create fizzbuzz.rs and run it"
```

Sign in with ChatGPT once through Codex so `~/.codex/auth.json` exists:

```sh
codex login
```

## Usage

```sh
cargo run -- "List the files and explain what this project does"
cargo run -- --model gpt-5.5 "Add tests for the CLI argument parser"
cargo run -- --transport sse "Use the HTTP/SSE transport instead"
cargo run -- --auth api-key "Use OPENAI_API_KEY instead"
```

`gpt-5.5` is the default model name used by this demo for the Codex backend; use
`--model` to choose a model your backend/account exposes. For `--auth api-key`,
set `OPENAI_API_KEY` first.

After installing:

```sh
cargo install --path crates/nav-cli
nav "List the files and explain what this project does"
nav --model gpt-5.5 "Add tests for the CLI argument parser"
nav --transport sse "Use the HTTP/SSE transport instead"
nav --auth api-key "Use OPENAI_API_KEY instead"
```

## TUI mode

`cd` to your project and run `nav` with no arguments — when stdout is a tty
and `--json-events` is not set, it launches the interactive TUI: chat
transcript above, multi-line composer below.

```sh
cd ~/code/my-project
nav
```

Pass a prompt to seed the first turn and skip having to type it in the
composer:

```sh
nav "kick off the session"
```

Inside the TUI:

- `Enter` submits, `Shift+Enter` inserts a newline.
- `Ctrl+U` clears to start of line, `Ctrl+W` deletes the previous word.
- `Up` / `Down` recall earlier prompts from this session.
- A leading `/` opens a slash-command popup (`/help`, `/clear`, `/quit`,
  `/resume`, `/sessions`). Type to filter, Tab/Enter completes.
- `/quit` and `Ctrl+C` twice exit cleanly; `/clear` empties the transcript.

To bypass the TUI and stream raw events:

```sh
nav --json-events "list the files" > events.ndjson
```

Each line is one `AgentEvent` as JSON (`assistant_message_delta`,
`tool_call_started`, `file_change`, `turn_diff`, `turn_complete`, …). Non-tty
stdout defaults to this mode automatically.

## Sessions

Every run is persisted to a local SQLite database (default
`$XDG_DATA_HOME/nav/nav.db`, falling back to `~/.local/share/nav/nav.db`).
Absolute `--db-path` values are honored; relative values are resolved inside the
nav data directory.

```sh
nav --list-sessions                 # all sessions, newest first
nav --list-sessions --cwd "$PWD"    # only sessions started in this directory
nav --resume <session-id> "follow-up prompt"
```

`--resume` rebuilds the Responses transcript from the on-disk event log and
appends the new prompt as the next turn. Token rollups appear in
`--list-sessions`; cost is shown only for providers that report it (none do
today, so the column reads `—`).

## Desktop UI

`nav-desktop` is the early Electron desktop shell for `nav`. It is intentionally
small for now: a left sidebar, a main prompt area, and a persisted working-directory
picker. If no workspace is selected, submitting from the prompt asks for one
first, so the future Rust agent loop has an explicit filesystem boundary before
it runs.

Install the UI dependencies once:

```sh
bun install
```

Start `nav-desktop`:

```sh
bun run start
```

## Tools

- `read_file`: read a relative file path
- `list_files`: list a relative directory
- `bash`: execute a shell command with a timeout
- `edit_file`: create a file or replace an exact string
- `apply_patch`: apply a reviewable multi-file patch with add/update/move/delete
  sections
- `code_search`: search with `rg`

Tool paths are resolved inside the current working directory.
Absolute paths, parent traversal (`..`), and symbolic-link escapes are rejected.
