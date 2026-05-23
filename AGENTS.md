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
`agent_loop/runner.rs` focused on the loop instead of accumulating
cross-cutting detail.

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

- The TUI uses an **inline viewport**, not the alternate screen. Finalized
  history cells are written into the terminal's native scrollback via
  `crates/nav-tui/src/insert_history.rs`; the ratatui viewport only paints
  composer + status + active streaming cell. There is no in-app scroll
  key — wheel / PgUp / PgDn are handled by the terminal itself. See
  [docs/tui-architecture-migration.md](docs/tui-architecture-migration.md)
  and [docs/tui-migration-plan.md](docs/tui-migration-plan.md) for the
  decision record and what's still deferred.
- TUI substrate gotchas worth knowing before editing the viewport plumbing:
  - `terminal.viewport_area` is zero-sized until `draw_tui` calls
    `set_viewport_area` for the first time. Anything that needs the column
    width *before* the first frame (`insert_history_lines`, scrollback
    wrap) must source it from `Backend::size()`, not the viewport.
  - The viewport is sticky-top, not bottom-anchored. `draw_tui` preserves
    `viewport_area.top()` so the first frame anchors at the shell-prompt
    row instead of slamming to the bottom of the screen and snapshotting
    the empty rows below into scrollback. When the viewport grows and would
    overflow the screen floor, the rows directly above it are pushed into
    native scrollback first (via
    `insert_history::scroll_region_above_into_scrollback`) — without that,
    streaming-expansion overwrites the user-prompt row.
  - On resize, nav does NOT re-emit cells into scrollback. The terminal
    handles re-wrapping its own scrollback at the new width. The previous
    `reflow_tail_lines` mechanism produced a duplicate transcript because
    re-emitted rows landed above identical old-width copies the terminal
    had already kept. A visible width seam at the resize point is by
    design.
  - `Terminal::set_viewport_area` resets the previous buffer whenever the
    viewport rect changes. `diff_buffers` compares cells by buffer index,
    not screen position — when the viewport's `y` shifts and a row holds
    the same content in both frames (e.g. the status bar at buffer index
    0), the diff would otherwise skip writing it and the new screen row
    would stay blank. If you touch viewport sizing or buffer flushing,
    keep this reset in place.
- **TUI changes require a tmux-backed regression test.** The in-process
  snapshot suite paints into a virtual `Buffer` and never exercises the
  diff/flush path that produces the actual terminal output, so visual
  regressions (status bar vanishes, cursor drifts, popup overpaints
  composer) slip past every other test. Any change that touches
  `app/render.rs`, `bottom_pane/`, `insert_history.rs`, or
  `custom_terminal.rs` must come with a test in
  `crates/nav-cli/tests/tmux_viewport.rs` (or a sibling file) that
  spawns the real `nav` binary in tmux, drives it through the
  scenario, and asserts on `tmux capture-pane -p` output.
  - **Pattern:** see `tmux_viewport.rs` — `Session` drop-guard for
    cleanup, `wait_for(predicate, timeout)` for polling instead of
    fixed sleeps, `env!("CARGO_BIN_EXE_nav")` to locate the built
    binary, `--auth api-key` + a throwaway `OPENAI_API_KEY` to bypass
    the `~/.codex/auth.json` read.
  - **Self-check:** before committing, temporarily revert the
    underlying fix and confirm the test fails with a useful diagnostic.
    A test that doesn't fail when the bug returns is worth nothing.
  - **Skip cleanly when tmux is absent** so CI hosts without it pass:
    `if !tmux_available() { eprintln!("..."); return; }`.
  - During development, also drive the TUI interactively in tmux and
    use `tmux capture-pane -p | nl -ba` to inspect what was painted.
    Resize between transitions (popup open/close, smaller/larger
    window) to surface buffer-diff bugs that identical-content cells
    can hide.
- `rg` must be on `PATH`; `code_search` shells out to it even though
  `Cargo.toml` does not mention it.
- `nav update` / `nav upgrade` downloads the latest tarball from GitHub
  Releases (Apple Silicon macOS only) and replaces the running binary in
  place via `std::env::current_exe()`. No Rust toolchain or source
  checkout required — see `crates/nav-cli/src/upgrade.rs`.
- Auth, transport, session storage, settings keys, and CLI defaults are
  documented in `README.md`; prefer linking there instead of duplicating them
  here.
- Three pair-shedding mechanisms coexist in the agent loop; compaction is
  the primary long-session strategy and pair-drop survives only as the
  in-compaction fallback. Order, with per-function doc comments that
  cross-reference each other:
  1. `prune::prune_to_budget` — proactive, before every sampling request.
  2. Normal-turn `ContextWindowExceeded` recovery in `runner.rs` — fires a
     full compaction and retries the turn once.
  3. `compaction_turn::trim_for_compaction` (and its primitive
     `drop_oldest_tool_pair`) — fallback inside a compaction turn that
     itself overflowed.

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
- Directory-backed Rust modules intentionally use the learning-oriented
  `foo/mod.rs` layout instead of the modern `foo.rs` + `foo/child.rs`
  convention. This keeps each module root inside its folder while Season is
  still building Rust fluency; revisit once idiomatic Rust navigation feels
  natural.
- Snapshot tests use `insta`; review pending snapshots with
  `cargo insta review` before committing.
- Commit messages should sound human, with short imperative subjects. Do not
  include `Co-Authored-By` trailers.
