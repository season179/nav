# TUI Migration — Implementation Plan

Concrete commit sequence for the decisions locked in
[`tui-architecture-migration.md`](./tui-architecture-migration.md). Each
phase is intended to be a self-contained branch/PR. Earlier phases land
shipping improvements; later phases polish.

Codex reference paths below are anchored at
`/Users/season/Personal/codex/codex-rs/tui/src/`.

## Status

| Phase | State | Notes |
|---|---|---|
| 1 — Foundation: inline viewport + scrollback insertion | **done** | Modules landed; wiring complete; tests green |
| 2 — Synchronized updates | **done** | BeginSynchronizedUpdate / EndSynchronizedUpdate brackets the drain + draw block |
| 3 — Resize reflow + re-insert | **done (basic)** | Re-emit-everything on resize; codex's debounce + row cap NOT ported |
| 4 — Streaming pipeline | **n/a** | Nav already streams into viewport, not scrollback — codex's commit-tick / table-holdback machinery doesn't apply |
| 5 — Composer / textarea | **deferred** | Independent project; nav's composer works on new substrate; would lose recent word-wrap improvements |
| 6 — Diff render + palette | **deferred** | codex's diff_render.rs is 92KB; orthogonal polish, not architectural |
| 7 — Overlays | **deferred** | codex's richer overlays are polish; existing overlays work on new substrate |
| 8 — Verify & polish | **done** | `cargo test --workspace`: 891 passed; `cargo build`: 0 warnings |

### What shipped

The architecture migration is complete: finalized chat history now lives
in the terminal's native scrollback above an inline viewport, not in a
ratatui frame. The deferred phases are codex-feature adoption (richer
composer, richer diff, richer overlays) and are independent follow-ups.

Concrete code in tree:
- `crates/nav-tui/src/custom_terminal.rs` — inline-viewport fork of
  `ratatui::Terminal`, ported from codex with adaptations for ratatui
  0.30.
- `crates/nav-tui/src/insert_history.rs` — scrollback-insert helper
  (simple char-wrap; URL-aware wrap deferred).
- `crates/nav-tui/src/app/terminal.rs` — no longer enters alt-screen or
  enables alt-scroll; only raw mode + bracketed paste + defensive mouse
  capture clear.
- `crates/nav-tui/src/app/render.rs` — viewport sized per-frame from
  composer + status + (optional) streaming row; bottom-anchored to the
  screen.
- `crates/nav-tui/src/widget.rs` — finalized cells live in a ledger
  (`finalized: Vec<Box<dyn HistoryCell>>`); `drain_pending` returns lines
  to push into scrollback; `reflow_all_lines` re-renders everything on
  resize; viewport `render` only paints the in-flight streaming cell.
- `crates/nav-tui/src/input/mod.rs` — `handle_scrollback_key` deleted;
  scrollback navigation is owned by the terminal.
- `crates/nav-tui/src/app/mod.rs` — main loop drains pending lines into
  `insert_history_lines` before each draw; both bracketed in
  `BeginSynchronizedUpdate` / `EndSynchronizedUpdate`; `Resize` events
  trigger a full reflow.
- `Cargo.toml` — `unicode-width = "0.2"` added.

### API drift note

Codex's `custom_terminal.rs` was written against ratatui 0.29 + a nornagon
fork. Nav uses ratatui 0.30 (stock). Differences handled during the port:

1. `Backend::Error` is now an associated type in 0.30. The port pins the
   custom `Terminal<B>` to `B: Backend<Error = io::Error> + Write`. Works
   for `CrosstermBackend` but not the broader generic codex used.
2. The `From<ratatui::Color> for crossterm::Color` impl was removed in
   0.30. Both files include a local `ratatui_to_crossterm` mapping
   function instead of `.into()`.
3. `WidgetRef` is gated behind the unstable `widget-ref` feature in
   ratatui 0.30. Nav's `Frame::render_widget_ref` method was dropped from
   the port; callers should use `Widget::render` directly.
4. `derive_more::IsVariant` replaced with a manual `is_put` method on
   `DrawCommand`.
5. `tracing` is not a nav dependency — the one `tracing::warn!` call on
   CPR failure was replaced with a silent fallback to origin.

### Pickup point for the next session

The remaining Phase 1 wiring (in order):

1. **`app/terminal.rs`** — drop `EnterAlternateScreen`,
   `LeaveAlternateScreen`, `EnableAlternateScroll`,
   `DisableAlternateScroll`. Keep raw mode, bracketed paste, defensive
   mouse-capture clear. Tests need updating to remove alt-screen /
   alt-scroll assertions.
2. **`app/mod.rs`** — swap `ratatui::Terminal::new(backend)` for
   `custom_terminal::Terminal::with_options(backend)`. Change
   `TerminalGuard.terminal` type to the new `Terminal`. Each loop tick,
   before `draw_tui`, drain pending history lines from `ChatWidget` and
   pass them to `insert_history::insert_history_lines(&mut term, lines)`.
3. **`app/render.rs`** — drop the `[Min(1) history, Length(pane_h)
   composer, Length(1) status]` layout. Replace with viewport that
   contains only composer + status (+ active streaming row). Call
   `term.set_viewport_area(rect)` each frame with the bottom-anchored
   rect.
4. **`widget.rs`** — refactor `ChatWidget`:
   - Add `pending_lines: Vec<Vec<Line<'static>>>`.
   - On every `push_cell` / `push_work_cell` / `push_local_cell`, render
     the cell once at a known width and append to `pending_lines`.
   - In `Widget for &ChatWidget`, render *only* the active streaming
     cell into the inline viewport.
   - Add `fn drain_pending(&mut self, width: u16) -> Vec<Line<'static>>`
     for the main loop to consume.
   - Delete: `CachedCell` + per-width cache, `scroll_top`, `scroll_up`,
     `scroll_down`, `scroll_to_top`, `scroll_to_bottom`,
     `scroll_anchor`, `rendered_height`, `extend_window`, viewport
     slicing in `Widget::render`.
5. **`input/mod.rs`** — drop the PgUp / PgDn / Home / End scrollback
   dispatch.
6. **Update tests** — `app/terminal.rs::tests` no longer asserts alt
   sequences. Snapshots that depend on the old full-frame widget will
   need `cargo insta review`.

Manual verification (no automated test will catch these):
- Wheel scroll = 1 line/notch (the original bug).
- Native text selection works.
- OS clipboard copy from finalized history works.
- After quit, the transcript remains in scrollback.
- Resize doesn't tear (defer to Phase 3 for the reflow story).

## Phase 1 — Foundation: inline viewport + scrollback insertion

Goal: end the alt-screen experiment. Move finalized cells into the
terminal's native scrollback. Keep only the composer + streaming + status
rows inside ratatui.

**Files added (nav-tui/src/):**
- `terminal/custom_terminal.rs` — minimal nav port of codex
  `custom_terminal.rs`. Tracks `viewport_area`, exposes
  `set_viewport_area`, `last_known_cursor_pos`, `draw` that paints only
  the viewport rect.
- `terminal/insert_history.rs` — nav port of the subset of codex
  `insert_history.rs` we need today: `insert_history_lines(terminal,
  Vec<Line>)`. Handles scroll region, MoveTo, RestorePosition.
- `terminal/mod.rs` — re-exports.

**Files modified:**
- `app/terminal.rs`
  - Drop `EnterAlternateScreen`, `LeaveAlternateScreen`,
    `EnableAlternateScroll`, `DisableAlternateScroll` from main entry.
  - Keep raw mode, bracketed paste, defensive mouse-capture clear.
  - `TerminalGuard` wraps the new `custom_terminal::Terminal`.
  - Update tests (drop the alt-screen / alt-scroll assertions).
- `app/render.rs`
  - Layout becomes: `[Min(1) streaming-or-empty, Length(pane_h)
    composer, Length(1) status]`.
  - Set the terminal's `viewport_area` to the union of those three
    blocks at the *bottom* of the screen; no "history pane" lives in
    ratatui.
- `widget.rs`
  - On `push_cell` / `push_local_cell` / `push_work_cell`, render the
    cell's lines and stage them as pending history lines (consumed by
    the next draw via `insert_history_lines`).
  - Streaming assistant cell stays rendered in the viewport until
    finalized; finalize → flush to scrollback.
  - Delete: `CachedCell` per-width cache, `scroll_top`, `scroll_up`,
    `scroll_down`, `scroll_to_top`, `scroll_to_bottom`,
    `scroll_anchor`, `rendered_height`, `extend_window`,
    full-transcript viewport slicing in `Widget for &ChatWidget`.
- `input/mod.rs` — drop PgUp/PgDn/Home/End scroll dispatch.

**Verify:**
- `cargo build -p nav-tui` clean.
- Manual: launch nav, type, see assistant streaming, finalize, scroll up
  with terminal native scrollback. Confirm wheel = 1 line/notch.
- Quit, confirm terminal scrollback retains the transcript.

## Phase 2 — Synchronized updates

Goal: prevent tearing when scrollback insertion races a viewport
redraw.

**Files modified:**
- `terminal/custom_terminal.rs` — wrap `draw` in
  `BeginSynchronizedUpdate` / `EndSynchronizedUpdate` (mirrors codex
  `tui.rs:30-50`).

## Phase 3 — Resize reflow + re-insert

Goal: when the terminal resizes, re-render finalized cells at the new
width and re-emit them into scrollback. Codex spends ~12KB on this.

**Files added:**
- `terminal/resize_reflow.rs` — port of codex
  `app/resize_reflow.rs` + `transcript_reflow.rs` +
  `resize_reflow_cap.rs`. Debounce SIGWINCH, walk the finalized cell
  ledger (a new field on `ChatWidget` holding `Vec<Box<dyn
  HistoryCell>>`), reflow at the new width, clear the old viewport,
  re-insert.

**Files modified:**
- `widget.rs` — retain a thin ledger of finalized cells purely for
  reflow (cell list is otherwise write-once).
- `app/mod.rs` — handle `Event::Resize` by scheduling a reflow.

## Phase 4 — Streaming pipeline

Goal: codex-style streaming so partial lines never enter scrollback.

**Files added:**
- `streaming/controller.rs`, `streaming/chunking.rs`,
  `streaming/commit_tick.rs`, `streaming/table_holdback.rs` — minimal
  ports.

**Files modified:**
- `widget.rs` / `streaming.rs` — replace current stable+tail rule with
  the codex queue + commit-tick + table-holdback. `AssistantMessageDone`
  flushes the controller's final buffer and emits one finalize+insert.

## Phase 5 — Composer / textarea adoption

Goal: codex's textarea (Vim-mode-ready, Ctrl+R reverse search,
paste-burst handling, persistent cross-session history).

**Files added/replaced:**
- `bottom_pane/textarea.rs` — from codex.
- `bottom_pane/paste_burst.rs` — from codex.
- `bottom_pane/chat_composer_history.rs` — from codex.

**Files modified:**
- `bottom_pane/composer.rs` — wraps the textarea; preserves nav
  attachments + pending-input preview. The recent word-wrap improvement
  in this file migrates into the textarea wrap policy.

## Phase 6 — Diff render + palette detection

Goal: pretty unified diffs with syntax highlighting and theme
adaptation.

**Files added:**
- `diff_render.rs`, `terminal_palette.rs`, `color.rs` — from codex.

**Files modified:**
- `cells/changes.rs`, `cells/tools.rs` — call `diff_render::render(...)`
  on the cell's display path.
- `theme.rs` — adopt codex's palette struct.

## Phase 7 — Overlays on the new substrate

Goal: approvals, mentions, slash popups gain codex's richer rendering.

**Files added/replaced:**
- `bottom_pane/approval_overlay.rs`, `bottom_pane/mentions_v2/` (subset),
  `bottom_pane/command_popup.rs` — adapted from codex.

**Files modified:**
- `bottom_pane/approval.rs`, `mention_popup.rs`, `slash_popup.rs` —
  thin nav-specific shells around the codex primitives.

## Phase 8 — Verify & polish

- `cargo build`, `cargo test`, `cargo insta review`.
- Manual: golden path turn-with-tool-call, streaming, approval, paste,
  attachments, slash, mention, resize.
- Update `docs/ARCHITECTURE.html` to describe the new substrate.
- Note in `CLAUDE.md` that history cells write to scrollback, not to a
  ratatui frame.

## Out-of-scope (DEFER, revisit individually)

- Ctrl+T pager overlay (`pager_overlay.rs`).
- Codex's resume / theme / keymap pickers.
- Side conversations, thread goals, multi-thread switching.
- Vim mode, external-editor integration.
- Plans / MCP / search-result cells.
- Tooltips, key-hint overlay, ambient pets, OSC 9 notifications.
