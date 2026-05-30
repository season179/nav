The nav TUI lives in `tui/` (Ink + React + TypeScript). It talks to the Rust
backend over local HTTP JSON-RPC and SSE — same protocol as before.

integration; wire components to `tui/src/backend/client.ts` instead.

Before finishing, run the linter and formatter and make sure everything passes.
