# nav

`nav` is a learning project: a coding agent built from the ground up to make
the agent loop, tool execution, permissions, context handling, and terminal UI
easier to understand by building them directly.

The primary purpose is learning by building a coding agent. The secondary
purpose is to grow it into a personalized coding agent for my own workflow.

Do not depend on `nav` yet. The project is early, experimental, and likely to
change drastically as the architecture becomes clearer.

## Local Backend Fixture

There is a minimal local HTTP/SSE backend fixture for frontend spikes:

```sh
cargo run --bin nav-local-backend -- --bind 127.0.0.1:0
```

See [docs/local-backend.md](docs/local-backend.md) for the printed URL contract,
fixture session ID, SSE event shape, and curl verification path.

## Electron Spike

The read-only Electron spike can render that fixture stream:

```sh
bun install
bun run electron:dev
```

See [docs/electron-spike.md](docs/electron-spike.md) for the boundary and smoke
verification path.

## License

MIT. See `LICENSE`.
