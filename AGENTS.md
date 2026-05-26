The nav TUI lives in `tui/` (Ink + React + TypeScript). It talks to the Rust
backend over local HTTP JSON-RPC and SSE — same protocol as before.

For UI patterns you can still skim ../crush, but do not copy its Go backend
integration; wire components to `tui/src/backend-client.ts` instead.
