# nav

`nav` is a learning project: a coding agent built from the ground up to make
the agent loop, tool execution, permissions, context handling, and terminal UI
easier to understand by building them directly.

The primary purpose is learning by building a coding agent. The secondary
purpose is to grow it into a personalized coding agent for my own workflow.

Do not depend on `nav` yet. The project is early, experimental, and likely to
change drastically as the architecture becomes clearer.

## Shape

- `tui/cmd/nav` is the user-facing command. Running `nav` starts the TUI.
- `crates/nav-backend` is the Rust backend process.
- The target frontend/backend API is JSON-RPC over local HTTP plus typed SSE
  events.

This keeps terminal rendering in Go/Bubble Tea while keeping agent state and
side effects in Rust. See `docs/architecture.md`.

## Development

Versions use CalVer: `YY.M.PATCH`. The first version after the rewrite is
`26.5.12`.

From the repository root:

```sh
cargo test
cd tui && go test ./...
cd tui && go run ./cmd/nav
```

`nav` is the released command. `navd` is a development launcher that runs the
locally built `target/debug/nav` with the locally built backend. Use
`navd update` from this checkout to rebuild local state and install the launcher:

```sh
make navd-update
navd
```

`navd update` builds `target/debug/nav-backend`, `target/debug/nav`, and
`target/debug/navd`, then installs only the launcher to `~/.local/bin/navd`.

Current prototype note: until the HTTP/SSE backend lands, the TUI still starts
the backend over stdio and only performs a hello check. By default it finds the
Rust workspace and runs:

```sh
cargo run --quiet --manifest-path Cargo.toml -p nav-backend -- serve
```

Set `NAV_BACKEND=/path/to/nav-backend` to point the TUI at a prebuilt backend.

## License

MIT. See `LICENSE`.
