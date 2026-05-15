# nav

`nav` is a small Rust coding agent built on the Responses API.

This keeps the spirit of Geoffrey Huntley's workshop: a coding agent is a loop
that can read, edit, search, run commands, and report back. The project started
as an educational implementation, but now also has a second goal: become a
usable local coding agent with a real product surface. Keep the simple learning
path intact, but prefer designs that can grow into reliable day-to-day use.

See [CONTEXT.md](CONTEXT.md) for the current product direction and engineering
priorities.

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
cargo install --path .
nav "List the files and explain what this project does"
nav --model gpt-5.5 "Add tests for the CLI argument parser"
nav --transport sse "Use the HTTP/SSE transport instead"
nav --auth api-key "Use OPENAI_API_KEY instead"
```

## Tools

- `read_file`: read a relative file path
- `list_files`: list a relative directory
- `bash`: execute a shell command with a timeout
- `edit_file`: create a file or replace an exact string
- `code_search`: search with `rg`

Tool paths are resolved inside the current working directory.
Absolute paths, parent traversal (`..`), and symbolic-link escapes are rejected.
