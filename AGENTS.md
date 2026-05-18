# AGENTS.md

Non-obvious context for editing `nav`. Product direction lives in
[docs/CONTEXT.md](docs/CONTEXT.md); a guided code tour lives in
[docs/ARCHITECTURE.html](docs/ARCHITECTURE.html).

## Reference implementations

Sibling coding-agent repos live next to this checkout — consult them before
inventing a pattern from scratch. They are read-only references; do not edit
them from a `nav` task.

- `../codex` — upstream Codex CLI; the canonical source for transport, auth,
  and `AgentEvent` shapes that `nav` mirrors.
- `../opencode` — alternative TUI/runtime architecture; useful for session
  persistence and frontend wire-format ideas.
- `../hermes-agent` — agent loop, tool-call plumbing, and skill execution
  patterns.
- `../nanoclaw` — minimal Claude-compatible harness; good for comparing the
  bare-minimum surface area.
- `../pi` — adjacent agent project; check for shared conventions before
  diverging.

## Prerequisites

- `rg` (ripgrep) on `PATH` — the `code_search` tool shells out to it and
  nothing in `Cargo.toml` will tell you this.
- Desktop UI uses **`bun`**, not npm: `bun install && bun run start`.

## Runtime defaults that are easy to miss

- **Auth defaults to ChatGPT OAuth** from `~/.codex/auth.json` (run `codex
  login` once). For a raw key, pass `--auth api-key` and set `OPENAI_API_KEY`.
- **Transport defaults to WebSocket** against the Codex Responses backend.
  `--transport sse` switches to streamed HTTP (kept for learnability).
- **TUI vs. NDJSON is auto-selected.** TTY stdout + no `--json-events` →
  interactive TUI. Anything else → one `AgentEvent` per line of NDJSON on
  stdout — the wire format every non-Rust frontend consumes.
- **Sessions persist to SQLite** at `~/Library/Application Support/nav/nav.db`
  (macOS) or `$XDG_STATE_HOME/nav/nav.db` (Linux). `--db-path` overrides it.
- **`nav update` / `nav upgrade`** reinstalls from the compile-time
  `CARGO_MANIFEST_DIR`, not from `$PWD`. If that checkout moved, the upgrade
  fails loudly instead of silently using a stale path.

## Skills and filesystem boundaries

- Skill discovery is scoped to **launch cwd only** — no upward walk to
  ancestors. Project skills (`.agents/skills/`) shadow user skills
  (`~/.agents/skills/`) by parsed `name`; the shadow is logged with both paths.
- **Writes are workspace-only.** `edit_file` rejects absolute paths, `..`, and
  symlink escapes. Reads under any catalog `skill_dir` are allowed but writes
  are not — that asymmetry is intentional, not a bug to fix.

## Conventions

- Versioning is **CalVer** in `[workspace.package].version`. Don't bump it
  alongside unrelated changes.
- Snapshot tests use `insta` — review pending snapshots with `cargo insta
  review` before committing.
- Commit messages: plain human voice, short imperative subjects (recent style:
  `Scope skill discovery to launch cwd only`). **No `Co-Authored-By` trailers.**
