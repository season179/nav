# AGENTS.md

Non-obvious guidance for agents editing `nav`. For broader product direction,
read [docs/CONTEXT.md](docs/CONTEXT.md). For a code tour, read
[docs/ARCHITECTURE.html](docs/ARCHITECTURE.html). Keep this file short:
repo-specific gotchas only.

## Core Shape

When changing `nav-core`, fit new behavior into these six harness parts
whenever possible:

1. **Tool registry**: model-visible tool definitions, tool access policy,
   dispatch, and concrete tool adapters.
2. **Model**: provider auth, request submission, streaming transport,
   response collection/parsing, usage extraction, and model-name handling.
3. **Context management**: project context, skills, extensions, replay,
   attachments, compaction, session history, and `/context` measurement.
4. **Guardrails**: approval policy, protected reads/writes, command
   classification, sandbox selection, and path-safety rules.
5. **Agent loop**: prompt intake, model/tool iteration, event emission,
   steering/abort handling, and turn lifecycle.
6. **Verify**: mutation summaries, turn diffs, doctor checks, test/command
   evidence, and future structured verification output.

Prefer locality: put new behavior behind the part that owns it, and keep
`agent/runner.rs` focused on the loop instead of accumulating cross-cutting
detail. See [docs/six-part-agent-harness-refactor.md](docs/six-part-agent-harness-refactor.md)
for the current refactor plan.

## Read-Only References

Sibling coding-agent repos are reference implementations only; do not edit them
from a `nav` task. In temporary worktrees they may not be literally next to this
path, so verify the real local checkout before assuming one is absent.

- `../codex`: canonical transport, auth, and `AgentEvent` shapes.
- `../opencode`: TUI/runtime architecture, persistence, wire-format ideas.
- `../kimiflare`: custom slash commands, command rendering, remote execution,
  sandboxing, branch/PR handoff.
- `../hermes-agent`: agent loop, tool-call plumbing, skill execution patterns.
- `../nanoclaw`: minimal Claude-compatible harness surface.
- `../pi`: adjacent agent conventions and shared local-tooling patterns.

## Local Gotchas

- `rg` must be on `PATH`; `code_search` shells out to it even though
  `Cargo.toml` does not mention it.
- `nav update` / `nav upgrade` reinstalls from compile-time
  `CARGO_MANIFEST_DIR`, not from the current working directory.
- Auth, transport, session storage, settings keys, and CLI defaults are
  documented in `README.md`; prefer linking there instead of duplicating them
  here.

## Scope and Safety Rules

- Skill, context-file, extension, and project-setting discovery are scoped to
  the launch cwd plus the user-scope fallback. Do not reintroduce an upward walk
  without updating the documented product rule.
- `AGENTS.md` and `CLAUDE.md` are deduped by canonical path; in this checkout
  `CLAUDE.md` is a symlink to `AGENTS.md`.
- Writes are workspace-only. `edit_file` rejects absolute paths, `..`, and
  symlink escapes. Reads under catalog `skill_dir`s are allowed; writes there
  are not.
- Writes to `.git`, `.agents`, and `.nav` are blocked regardless of approval
  mode. Reads of `.env*`, `*.pem`, `*.key`, and SSH keys require approval.
- Keep safety behavior easy to audit. Guardrail changes need focused tests for
  path containment, protected metadata, approval decisions, and sandbox policy.

## Conventions

- Versioning is CalVer in `[workspace.package].version`; do not bump it for
  unrelated changes.
- Snapshot tests use `insta`; review pending snapshots with
  `cargo insta review` before committing.
- Commit messages should sound human, with short imperative subjects. Do not
  include `Co-Authored-By` trailers.
