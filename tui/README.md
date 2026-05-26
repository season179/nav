# nav TUI (Ink)

React-for-the-terminal UI for nav. The backend protocol client is
`src/backend-client.ts` (JSON-RPC + SSE).

## Run

```sh
bun install
bun run start
```

From the repo root: `navd` (after `navd update`) or `make run-tui`.

## Preview UI (no backend)

Iterate on layout and styling without `navd update`, `nav-backend`, or an LLM:

```sh
cd tui
bun run preview              # full shell with mock messages
bun run preview history      # history pane only
bun run preview composer     # composer only
bun run preview model        # /model picker only
```

In the default `shell` preview: `m` opens the model picker, `e` clears history,
`c` restores sample chat, `q` quits. `/model` and `/exit` work in the input like
the real app.

## Composer regression tests

Fast, headless checks that the composer keeps its Claude-style layout (full-width
`─` rules, `>` prompt, hint row). No backend or tmux required:

```sh
cd tui
bun test
```

- **Layout invariants** — row count, rule width, `>` prefix, hint text
- **Golden snapshots** — stable frames when text is visible (busy / typed)
- **Regression guards** — no round borders, no removed placeholder copy

Update snapshots intentionally after visual changes: `bun test --update-snapshots`.

## Backend

By default the UI spawns `cargo run -p nav-backend -- serve-http` from the
nearest `Cargo.toml` that lists `nav-backend`. Override with:

```sh
export NAV_BACKEND=/path/to/nav-backend
export NAV_MODEL_SETTINGS=/path/to/settings.json
```
