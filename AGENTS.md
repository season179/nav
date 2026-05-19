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
- **Sessions persist to SQLite** at `$XDG_DATA_HOME/nav/nav.db`, falling back
  to `~/.local/share/nav/nav.db`. Absolute `--db-path` overrides it; relative
  values resolve inside the nav data directory.
- **`nav update` / `nav upgrade`** reinstalls from the compile-time
  `CARGO_MANIFEST_DIR`, not from `$PWD`. If that checkout moved, the upgrade
  fails loudly instead of silently using a stale path.
- **Approval policy defaults to `on-request`.** Classifier-flagged
  dangerous commands prompt the operator before running; unbypassable
  patterns (`sudo`, `rm -rf /`, fork bomb, `mkfs*`, etc.) are refused even
  with `--dangerously-bypass-approvals-and-sandbox`. CLI flags:
  `--approval-policy {untrusted,on-request,never}` and
  `--sandbox {read-only,workspace-write,danger-full-access}`.
- **Sandbox defaults to `workspace-write`.** On macOS this is enforced via
  `sandbox-exec` with an embedded `.sbpl` profile that allows reads
  anywhere, writes only under the workspace root, and gates network. On
  Linux/Windows the sandbox is currently passthrough — the classifier and
  protected-metadata rules still apply.
- **`.git`, `.agents`, `.nav` writes are blocked** regardless of approval
  mode. Reads of `.env*`, `*.pem`, `*.key`, and SSH keys require approval
  even when the path is in-tree.
- **NDJSON approval reverse channel.** In `--json-events` mode with a piped
  stdin, the agent reads JSON lines of the form
  `{"kind":"approval_response","approval_id":"…","decision":"approved"}`.
  On a TTY stdin we auto-downgrade to `--approval-policy never` and warn.

## Skills and filesystem boundaries

- Skill discovery is scoped to **launch cwd only** — no upward walk to
  ancestors. Project skills (`.agents/skills/`) shadow user skills
  (`~/.agents/skills/`) by parsed `name`; the shadow is logged with both paths.
- **Project context (`AGENTS.md`, `CLAUDE.md`) follows the same rule.**
  Discovery is cwd-only at `<launch_cwd>/{AGENTS.md,CLAUDE.md}` plus a
  user-scope fallback at `~/.agents/{AGENTS.md,CLAUDE.md}`. Files are deduped
  by canonical path (so a `CLAUDE.md → AGENTS.md` symlink loads once) and
  prepended to the Responses API `instructions` field in user-then-project
  order. Set `disable_context_files: true` in `.nav/settings.json` to skip
  this entirely.
- **Project settings live at `<launch_cwd>/.nav/settings.json` and
  `~/.nav/settings.json`.** Same scoping: no upward walk. Project overrides
  user; explicit CLI flags beat both. Schema is the subset of CLI flags that
  make sense as defaults: `model`, `auth`, `transport`, `max_turns`,
  `bash_timeout_secs`, `disable_context_files`. Unknown keys reject the file
  with an eprintln; malformed JSON falls back to defaults (startup never
  blocks on broken settings).
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
