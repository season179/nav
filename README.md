# nav

A tiny Rust coding agent built on the Responses API.

This keeps the spirit of Geoffrey Huntley's workshop: a coding agent is a loop
that can read, edit, search, run commands, and report back. By default it uses
the ChatGPT OAuth credentials stored by Codex in `~/.codex/auth.json` and calls
the Codex Responses backend directly.

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
cargo run -- --auth api-key "Use OPENAI_API_KEY instead"
```

After installing:

```sh
cargo install --path .
nav "List the files and explain what this project does"
nav --model gpt-5.5 "Add tests for the CLI argument parser"
nav --auth api-key "Use OPENAI_API_KEY instead"
```

## Tools

- `read_file`: read a relative file path
- `list_files`: list a relative directory
- `bash`: execute a shell command with a timeout
- `edit_file`: create a file or replace an exact string
- `code_search`: search with `rg`

Tool paths are resolved inside the current working directory.
