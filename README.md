# nav

`nav` is a learning project: a coding agent built from the ground up to make
the agent loop, tool execution, permissions, context handling, and terminal UI
easier to understand by building them directly.

The primary purpose is learning by building a coding agent. The secondary
purpose is to grow it into a personalized coding agent for my own workflow.

Do not depend on `nav` yet. The project is early, experimental, and likely to
change drastically as the architecture becomes clearer.

## Local Chat Backend

There is a minimal local HTTP/SSE backend that runs an in-memory, multi-turn
chat loop backed by one text model:

```sh
NAV_MOCK_MODEL=1 cargo run --bin nav-local-backend -- --bind 127.0.0.1:0
```

It exposes `session.create` and `session.sendMessage` over `POST /rpc` and a
live session event stream at `GET /sessions/{id}/events`. See
[docs/local-backend.md](docs/local-backend.md) for the URL contract, model
configuration (`NAV_MOCK_MODEL`, `NAV_API_KEY`, `NAV_MODEL`, `NAV_BASE_URL`),
SSE event shape, and a curl verification path.

## Electron Chat Spike

The Electron app is a minimal multi-turn chat window over that backend:

```sh
bun install
NAV_MOCK_MODEL=1 bun run electron:dev   # or set NAV_API_KEY for a real model
```

See [docs/electron-spike.md](docs/electron-spike.md) for the security boundary
and the manual real-model verification path.

## License

MIT. See `LICENSE`.
