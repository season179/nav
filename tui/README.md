# nav TUI (Ink)

React-for-the-terminal UI for nav. The backend protocol client is
`src/backend-client.ts` (JSON-RPC + SSE).

## Run

```sh
bun install
bun run start
```

From the repo root: `navd` (after `navd update`) or `make run-tui`.

## Backend

By default the UI spawns `cargo run -p nav-backend -- serve-http` from the
nearest `Cargo.toml` that lists `nav-backend`. Override with:

```sh
export NAV_BACKEND=/path/to/nav-backend
export NAV_MODEL_SETTINGS=/path/to/settings.json
```
