# TUI Architecture Migration — Follow Codex

## Why this exists

Nav's TUI renders the entire transcript inside a ratatui frame inside the
alternate screen. That choice surfaced as a small bug (mouse-wheel
scrolling moves 3 lines per notch instead of 1) and a broader limitation:
text selection requires fighting the terminal, scrollback lives only in
process memory, and resize reflow has to be reinvented inside ratatui.

Codex solves all three at once by giving up on rendering history inside
ratatui at all. Finalized turns are written above the inline viewport
into the terminal's native scrollback via escape sequences; only the
composer and the active streaming turn live inside a ratatui viewport.
Wheel scroll, text selection, OS-level copy, and clipboard integration
all work because the terminal owns those rows.

This plan is the bet: adopt codex's architecture as nav's TUI foundation,
so future feature work (richer cell types, better diff rendering, pager
overlays, theme support) is layered on a substrate we know scales.

## Universal decisions

Every feature in the inventory below carries one of five marks. The
decisions are blanket:

| Mark | Meaning | Action |
|---|---|---|
| `COPY` | Direct match between nav and codex. | Replace nav's implementation with codex's. |
| `FOLLOW` | Similar concept, different details. | Adopt codex's design; let details follow. |
| `PORT` | Nav-unique feature worth keeping. | Rebuild on the new architecture; preserve behavior. |
| `DELETE` | Feature the new architecture makes obsolete. | Remove the code; no replacement. |
| `DEFER` | Codex-only feature nav does not need today. | Skip for now; revisit individually later. |

Anything in `DEFER` that turns out to be part of the infrastructure we
adopt (e.g. synchronized updates, reflow debounce) gets promoted to
`COPY` because it's not really optional — it's how codex's architecture
works.

## 1. Display modes

- `FOLLOW` — **Single alt-screen for everything** (`app/terminal.rs:23-29`)
  → split into inline viewport for composer + active turn, alt-screen
  reserved for the future `Ctrl+T` pager and pickers (codex `tui.rs:60-150`,
  `pager_overlay.rs:1-90`). Core change.
- `FOLLOW` — **Three-pane layout** (`app/render.rs:37-42`) → the
  "history pane" disappears; only composer + status + optional
  streaming-turn pane remain.
- `DEFER` — Codex alt-screen pickers (resume, theme, keymap).

## 2. History rendering

- `PORT` — **Cell-based architecture** (`history.rs`, `cells/*.rs`) ↔ codex
  `history_cell/*.rs`. Keep nav's cells; change the rendering target.
- `DELETE` — **Cached line layout per width** (`widget.rs:19-66`). Cells
  stop re-rendering on scroll; only on resize-reflow.
- `DELETE` — **Full-height transcript in-frame** (`widget.rs:549-600`).
  Replaced by `insert_history_lines`-style escape sequences (codex
  `insert_history.rs:45-120`).
- `PORT` — **Streaming assistant cell held outside `cells` vec**
  (`widget.rs:96-106, 207-225`). Stays in inline viewport; on finalize,
  appended to scrollback.
- `PORT` — **Local-only control-plane cells (PendingInputCell)**
  (`widget.rs:495-502`). Write to scrollback like normal cells — they are
  informational events.
- `DEFER` — Codex's plans, MCP, separators, search results,
  request-user-input cells.

## 3. Streaming

- `FOLLOW` — **Stable+tail partition rule** (`streaming.rs:41-69`) →
  adopt codex's pipeline (`streaming/mod.rs`, `controller.rs`,
  `chunking.rs`, `commit_tick.rs`, `table_holdback.rs`). Codex's queue,
  commit-tick policy, and table holdback all matter when streaming into
  scrollback because partial lines cannot be unwritten.
- `PORT` — **AssistantMessageDone coalescing** (`widget.rs:216-225`)
  becomes "finalize streaming cell + insert into scrollback."

## 4. Scrolling

- `DELETE` — **Keyboard scroll keys** (`input/mod.rs:25-47`). Terminal
  handles all scrollback navigation natively. *Direct fix for the
  3-line wheel bug.*
- `DELETE` — **`scroll_top: Option<usize>` state**
  (`widget.rs:104-105`).
- `DELETE` — **Scroll anchor math, rendered_height walk, viewport
  slicing** (`widget.rs:466-480, 72-86, 549-600`).
- `DELETE` — **Scroll-to-bottom on new input** (`widget.rs:407-427`).
  Terminal handles it.
- `DEFER` — Codex's pager-overlay PgUp/PgDn/arrow scroll
  (`pager_overlay.rs:150-250`). Copy from there if we add a `Ctrl+T`
  overlay.

## 5. Composer / input

- `COPY` — **Multiline buffer + Bash-style keybindings**
  (`bottom_pane/composer.rs`) ↔ codex `bottom_pane/textarea.rs`. Adopt
  codex's textarea so future features (Vim mode, reverse-search) plug
  in.
- `COPY` — **Shift+Enter inserts newline** (already at
  `composer.rs:258-259`) ↔ codex `textarea.rs`. Behavior matches;
  preserve when migrating to codex's textarea.
- `COPY` — **Command history Up/Down + pending_draft**
  (`composer.rs:41-43`) ↔ codex `chat_composer_history.rs`. Codex's
  version adds Ctrl+R reverse-search and persistent cross-session
  history — accept both as part of the copy.
- `COPY` — **Large paste placeholder** (`composer.rs:28-32, 47, 232-250`)
  ↔ codex `chat_composer.rs:60-100`, `paste_burst.rs`. Adopt codex's
  paste-burst handling for Windows compatibility while we're here.
- `PORT` — **Pending attachments (image paste)** (`composer.rs:73-82`).
  Codex's textarea handles remote images too; rebuild nav's local-file
  attachments on codex's element model.
- `DEFER` — Vim mode.
- `DEFER` — External-editor integration (`external_editor.rs`).

## 6. Slash commands

- `COPY` — **Slash popup with fuzzy filter, max-visible cap,
  esc-suppression** (`bottom_pane/slash_popup.rs`) ↔ codex
  `bottom_pane/command_popup.rs:35-100`.
- `PORT` — **Skill wrapping `<skill name=… dir=…>`**
  (`input/slash.rs:35-111`). Nav-specific (agentskills.io). Rebuild on
  codex's command-dispatch surface.
- `PORT` — **Prompt templates `/prompt:<name>`** (`input/slash.rs:51-76`).
- `PORT` — **Control commands** `/steer`, `/queue-edit`, `/queue-remove`,
  `/queue-clear`, `/abort` (`input/slash.rs:113-134`).
- `PORT` — **Session commands** `/sessions`, `/tree`, `/fork`, `/find`,
  `/checkpoint`, `/stash`, `/restore`, `/handoff`, `/compact`, `/label`,
  `/unlabel`.
- `DEFER` — Codex `/plugins`, `/apps`, `/realtime`, `/theme`, `/memory`,
  `/personality`, `/plan`, `/review`.

## 7. Approvals / permissions UI

- `COPY` — **Approval modal overlay + queue + audit trail**
  (`bottom_pane/approval.rs`, `app/permissions.rs`) ↔ codex
  `bottom_pane/approval_overlay.rs:1-200`. Adopt codex's richer per-type
  rendering (network context, syntax-highlighted diffs) as part of the
  copy.
- `PORT` — **Bypass mode** (`app/permissions.rs:18-24`). Nav-specific
  flag.

## 8. Tool-call rendering

- `COPY` — **Cell types for tool-call started / output / context-paired**
  (`cells/tools.rs`, `widget.rs:93, 268-279`) ↔ codex `exec_cell/*`.
- `FOLLOW` — **Output preview truncation** (`cells/tools.rs:11-13`) ↔
  codex `exec_cell/render.rs:1-50`. Same idea, codex's caps and
  truncation marker.
- `COPY` — **Unified diff rendering with theme adaptation and syntax
  highlighting** — adopt codex's `diff_render.rs` wholesale. This is
  one of the bigger wins of "follow codex": prettier diffs come along
  with the architecture.

## 9. Session management UI

- `PORT` — **`SessionListCell`, `SessionTreeCell`, `SessionNoticeCell`,
  `TranscriptHitsCell`** (`cells/sessions.rs`). Become normal scrollback
  cells.
- `PORT` — **Session picker popup overlay**
  (`bottom_pane/session_picker.rs`). Restructure on codex's overlay
  primitives; do not adopt codex's full alt-screen resume picker yet —
  that is in `DEFER`.
- `PORT` — **Rewind UI** (`widget.rs:631-653`, `app/mod.rs:564-640`).
  Different protocol from codex's `app_backtrack.rs`; rebuild on the
  new view layer.
- `DEFER` — Codex side conversations, thread goals with token budgets,
  multi-thread switching.

## 10. Status indicators

- `COPY` — **Single-line status bar (model, cwd, git, dirty, agent
  state)** (`app/status_bar.rs`) ↔ codex `bottom_pane/footer.rs:1-50`.
- `COPY` — **Context gauge `X.Xk/YYYk ZZ%`** (`app/status_bar.rs:97-112`)
  ↔ codex `token_usage.rs`, `status/format.rs`.
- `COPY` — **Spinner + `Working <secs>s`** (`app/status_bar.rs:43-46`,
  `app/mod.rs:48`) ↔ codex `status_indicator_widget.rs:40-100`,
  `shimmer.rs`. Adopt codex's reduced-motion handling as part of the
  copy.
- `DEFER` — Codex rate-limit windows, account info, goal status.

## 11. Themes / colors

- `FOLLOW` — **Theme struct with 2 colors (composer_bg, popup_bg)**
  (`theme.rs:7-14`) → adopt codex's palette detection (`terminal_palette.rs`,
  `color.rs`) and light/dark adaptation. Follows naturally once we want
  codex's diff rendering.
- `PORT` — **Cell-level styling with row glyphs** (`cells/row.rs:14-104`).
  Keep nav's glyphs; render the same when written to scrollback.

## 12. Terminal lifecycle

- `COPY` — **enter_tui / leave_tui sequences** (`app/terminal.rs:23-35`)
  ↔ codex `tui.rs:25-50`. Restructure to codex's split between global
  lifecycle and alt-screen-only sequences.
- `COPY` — **Panic teardown hook** (`app/terminal.rs:37-44`) ↔ codex
  `tui.rs:82-120`.
- `COPY` — **Bracketed paste enable/disable** (`app/terminal.rs:46-68`)
  ↔ codex `tui.rs:20-40`.
- `DELETE` — **`EnableAlternateScroll` (`\x1b[?1007h`)**
  (`app/terminal.rs:72-111`). Remove from main entry; move to pager
  overlay only.
- `COPY` — **Synchronized updates** (codex `tui.rs:30-50`). Wrap frame
  draws in `BeginSynchronizedUpdate` / `EndSynchronizedUpdate` so
  scrollback insertions are atomic.
- `DEFER` — Focus events, kitty keyboard protocol, terminal title.

## 13. Resize handling

- `FOLLOW` — **Dynamic viewport recomputed per frame**
  (`app/render.rs:31-43`). Viewport becomes composer + status +
  streaming only.
- `COPY` — **Cell layout cache invalidation on width change**
  (`widget.rs:40-50`) → replaced by codex's reflow-and-re-insert: on
  SIGWINCH, walk finalized cells, re-render at new width, re-insert
  into scrollback (codex `app/resize_reflow.rs:1-120`,
  `transcript_reflow.rs`).
- `COPY` — **Reflow debounce + row cap** (codex `transcript_reflow.rs:1-50`,
  `resize_reflow_cap.rs`).

## 14. Clipboard

- `COPY` — **OS clipboard image paste → `.nav/clipboard/UUID.png`**
  (`bottom_pane/clipboard.rs:1-20`) ↔ codex `clipboard_paste.rs:50-100`.
- `PORT` — **File path / `file://` URL paste handling**
  (`bottom_pane/clipboard.rs:22-49`). Nav-specific path resolution into
  `.nav/clipboard/`.
- `PORT` — **Image dimension validation**
  (`bottom_pane/clipboard.rs:51-67`).
- `DEFER` — Codex Ctrl+O copy-last-response, OSC 52 / tmux / arboard /
  WSL backends, clipboard leases.

## 15. Other UI

- `PORT` — **Welcome cell** (`cells/welcome.rs`). Becomes a scrollback
  cell.
- `FOLLOW` — **`@file` mention popup with workspace file index**
  (`bottom_pane/mention_popup.rs`) → restructure on codex's
  `mentions_v2` primitives; keep nav's narrower catalog (files only).
- `PORT` — **Pending input queue preview**
  (`bottom_pane/pending_preview.rs`). Lives in bottom pane, not
  scrollback. Nav-specific feature.
- `PORT` — **Error, notice, approval-decision, file-change, turn-diff,
  git-checkpoint, compaction, subagent, skill-invocation, turn-aborted
  cells** (`cells/*.rs`). Write to scrollback.
- `DEFER` — Tooltips, key-hint overlay, model picker, reasoning-effort
  cycle, fast/vim/raw-scrollback toggles, quit-confirm, file-search
  popup, multi-select picker, custom prompt view, memories settings,
  experimental features, feedback view, hooks browser, MCP servers,
  status-surface preview, title setup, app-link OAuth, ambient pets,
  pet picker, OSC 9 / BEL notifications.

## Migration scope summary

| Change | Effort | Notes |
|---|---|---|
| Replace in-frame transcript with `insert_history` calls | **Large** | Core architectural change |
| Adopt codex's textarea (composer + Shift+Enter + paste-burst + history) | **Large** | Foundation for future input features |
| Delete scroll math + `scroll_top` state + cache | Medium | Direct cause of 3-line wheel bug |
| Restructure terminal lifecycle (drop alt-screen + alt-scroll from main entry) | Small | Direct fix; part of the new lifecycle pattern |
| Add resize reflow (re-render + re-insert finalized cells at new width) | **Large** | Codex spends 12 KB+ on this; non-trivial |
| Add synchronized updates around frame draws | Small | Prevents scrollback tearing |
| Restructure streaming cell on codex's pipeline (queue, commit-tick, table holdback) | **Large** | Required because partial scrollback lines cannot be unwritten |
| Adopt codex's diff rendering and palette detection | Medium | Comes with the architecture; visible polish win |
| Port all nav cell types onto scrollback rendering | Medium | Mechanical; many small touches |
| Port nav-specific slash commands, session UI, mention popup, clipboard | Medium | Behavior preserved on new substrate |
| Keep composer keys, approvals, status bar feature set | Zero | Already aligned conceptually |

## Codex reference files

The implementation plan will draw from these codex files (in
`/Users/season/Personal/codex/codex-rs/tui/src/`):

- `insert_history.rs` — escape-sequence insertion above the inline
  viewport with adaptive URL-aware wrapping.
- `custom_terminal.rs` — viewport boundaries, cursor tracking, what the
  terminal owns vs. ratatui.
- `tui.rs` and `tui/mod.rs` — lifecycle, alt-screen entry/exit,
  synchronized updates, panic hook structure.
- `app/resize_reflow.rs`, `transcript_reflow.rs`, `resize_reflow_cap.rs`
  — reflow + re-insert on resize, debounce, row cap.
- `streaming/mod.rs`, `streaming/controller.rs`, `streaming/chunking.rs`,
  `streaming/commit_tick.rs`, `streaming/table_holdback.rs` — streaming
  pipeline.
- `bottom_pane/textarea.rs`, `bottom_pane/chat_composer.rs`,
  `bottom_pane/paste_burst.rs`, `bottom_pane/chat_composer_history.rs`
  — composer foundation.
- `bottom_pane/command_popup.rs` — slash popup primitives.
- `bottom_pane/approval_overlay.rs` — approval modal rendering.
- `bottom_pane/mentions_v2/*` — mention popup primitives.
- `diff_render.rs`, `terminal_palette.rs`, `color.rs` — diff rendering
  and palette detection.
- `pager_overlay.rs` — only when we eventually add the `Ctrl+T`
  overlay.

## Next step

This document is decision-locked. The next artifact is an
implementation plan that breaks the scope above into ordered, shippable
slices (e.g. lifecycle restructure → scrollback insertion → cell
porting → streaming pipeline → resize reflow → composer adoption →
diff rendering) with branches/PR boundaries.
