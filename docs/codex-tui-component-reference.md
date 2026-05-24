# Codex TUI Component Reference

A web-developer-friendly guide to the Codex terminal UI architecture.

## Big picture

Codex's TUI is built on [ratatui](https://ratatui.rs/) (a Rust terminal UI
framework, similar in spirit to React for the terminal). It uses an **inline
viewport** instead of the alternate screen for the main chat — meaning the
conversation lives inside your normal terminal session, and finalized output
becomes part of your terminal's native scrollback. Some overlays (transcript
viewer via `Ctrl+T`, diff viewer, pager) do use the alternate screen.

The layout has three conceptual layers:

```
┌─────────────────────────────────────┐
│                                     │
│  Native scrollback   (the "DOM")    │  ← terminal-owned, Codex can't edit
│  - finalized history cells          │
│  - previous turns                   │
│                                     │
├─────────────────────────────────────┤
│  ratatui viewport                   │  ← Codex-owned, redrawn each frame
│  - streaming active cell            │
│  - hook cell (if running)           │
│  - ambient pet image (optional)     │
├─────────────────────────────────────┤
│  Bottom pane        (fixed bottom)  │  ← always visible
│  - status indicator (while working) │
│  - pending input preview            │
│  - chat composer input              │
│  - popups (approval, slash, etc.)   │
└─────────────────────────────────────┘
```

Overlays (transcript, diff, pager) are rendered on a separate alternate screen
and occupy the full terminal when active.

## Key architectural difference from web

There is **no virtual DOM or reconciliation**. Cells are **write-once**:

1. An event arrives from the app server session (protocol layer).
2. `ChatWidget` translates it into a cell (a struct implementing `HistoryCell`).
3. Once finalized, the cell is rendered into styled terminal lines and written
   into the terminal's native scrollback via escape sequences
   (`insert_history_lines`).
4. The TUI never touches that content again.

The ratatui viewport only paints the **active streaming cell** and the **bottom
pane** — typically 5–15 rows total. This keeps rendering fast and memory bounded
regardless of session length.

There is one exception: **resize reflow**. On terminal width changes, a
`TranscriptReflowState` can rebuild recent committed cells at the new width and
re-emit them into scrollback, but this is an explicit opt-in recovery path, not
continuous reconciliation.

## Cell types ("component library")

Cells are the atomic, finalized blocks in scrollback. Each is a Rust struct
that implements `HistoryCell` — think of it as a component with a single
`render()` method: `display_lines(width) -> Vec<Line>`.

Cells support two render modes:
- **Rich** (`display_lines`) — styled output with colors and formatting.
- **Raw** (`raw_lines`) — copy-friendly plain text for raw-output mode (`/raw`).

They also support a **transcript** representation (`transcript_lines`) used by
the `Ctrl+T` overlay, which can differ from the main viewport rendering (e.g.
`ExecCell` shows all calls with `$`-prefixed commands in transcript mode).

## Cell implementation priority

For `nav`, do not chase every Codex cell at once. Work cell-by-cell, starting
with the blocks that most affect the live redraw layer and the handoff from
ratatui viewport to terminal scrollback.

| Rank | Cell / surface | Why it matters first |
|---:|---|---|
| 1 | `AgentMessageCell` | Live assistant text while the model is streaming; this is the core nav-owned redraw-each-frame cell. |
| 2 | `ExecCell` / active tool-call cell | Makes agent work legible while commands and tools are running. |
| 3 | `AgentMarkdownCell` | Final assistant message after streaming; proves live text finalizes cleanly into scrollback. |
| 4 | `ReasoningCell` | Shows reasoning summaries, but should stay quiet and not dominate the transcript. |
| 5 | `HookCell` | Shows pre/post hook activity; important for the harness, but should remain subtle. |
| 6 | `UserHistoryCell` | Shows the submitted prompt; important, but simpler than assistant/tool cells. |
| 7 | `PatchHistoryCell` / file-change cell | Shows edit and diff summaries once verify/edit workflows are central. |
| 8 | `McpToolCallCell` | Useful if MCP/app tools become first-class in nav. |
| 9 | `PlanUpdateCell` | Useful for checklist updates when nav leans on explicit plans. |
| 10 | `ProposedPlanStreamCell` / `StreamingPlanTailCell` | Nice live-plan polish, but not core redraw infrastructure. |
| 11 | `ProposedPlanCell` | Final proposed-plan display; useful after streaming/message basics are solid. |
| 12 | `ApprovalDecisionCell` | Audit trail for approval outcomes; visually straightforward. |
| 13 | `ReviewDecisionCell` | Valuable once guardian/review workflows are active. |
| 14 | `RequestUserInputResultCell` | Useful after richer request-user-input support exists. |
| 15 | Warning / notice cells | Important, but generally straightforward banners. |
| 16 | `FinalMessageSeparator` | Turn divider and metrics polish; low architectural risk. |
| 17 | `UpdateAvailableHistoryCell` | Low-priority update notice. |
| 18 | Session header cells | Welcome/session context; not part of the main live redraw path. |
| 19 | Image-related cells | Useful later if image tools matter. |
| 20 | `WebSearchCell` | Only urgent if web search is a first-class nav workflow. |
| 21 | `UnifiedExecInteractionCell` | Powerful background-process interaction, but advanced; do not start here. |

Recommended first pass: `AgentMessageCell` → `ExecCell` →
`AgentMarkdownCell`. That covers the core lifecycle of live assistant text,
live tool activity, and finalized scrollback.

### Conversation cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `UserHistoryCell` | User chat bubble | `history_cell/messages.rs` | The prompt you typed, with text elements (mentions, image attachments) |
| `AgentMarkdownCell` | Assistant chat bubble | `history_cell/messages.rs` | Finalized markdown from the model (source-backed for resize reflow) |
| `AgentMessageCell` | Streaming chat bubble | `history_cell/messages.rs` | In-flight streaming assistant text (replaced by `AgentMarkdownCell` on consolidation) |
| `StreamingPlanTailCell` | Live preview block | `history_cell/plans.rs` | Transient preview of a proposed plan while streaming |
| `ProposedPlanStreamCell` | Live plan preview | `history_cell/plans.rs` | Streaming proposed-plan display |
| `ProposedPlanCell` | Finalized plan card | `history_cell/plans.rs` | Source-backed proposed plan (can re-render at new width) |
| `PlanUpdateCell` | Checkbox todo list | `history_cell/plans.rs` | `update_plan` checklist with step statuses |
| `ReasoningCell` | Collapsible think block | `history_cell/messages.rs` | Model reasoning/chain-of-thought output |

### Tool / exec cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `ExecCell` | Collapsible `<details>` | `exec_cell/model.rs` | One or more grouped shell commands with output (supports "exploring" grouping) |
| `McpToolCallCell` | Collapsible result block | `history_cell/mcp.rs` | MCP tool invocation with args, duration, and result |
| `PatchHistoryCell` | Diff view | `history_cell/patches.rs` | File-level diff summary of applied patches |
| `WebSearchCell` | Activity indicator | `history_cell/search.rs` | Web search tool activity with query and action state |
| `UnifiedExecInteractionCell` | Nested card | `history_cell/exec.rs` | Background terminal stdin interaction |

### Agent structure cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `HookCell` | Quiet status chip | `history_cell/hook_cell.rs` | Hook execution (intentionally quiet — only visible when slow or has output) |
| `RequestUserInputResultCell` | Q&A card | `history_cell/request_user_input.rs` | Completed request_user_input exchange showing questions and answers |

### System / status cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `ApprovalDecisionCell` | Inline prompt response | `history_cell/approvals.rs` | Approved / denied / timed-out tool execution |
| `ReviewDecisionCell` | Inline status badge | `history_cell/approvals.rs` | Guardian review outcomes |
| `FinalMessageSeparator` | Horizontal rule | `history_cell/separators.rs` | Visual divider between turns (shows duration and runtime metrics) |
| `UpdateAvailableHistoryCell` | Update toast card | `history_cell/notices.rs` | New version available notification |
| Warning / notice cells | Alert banner | `history_cell/notices.rs` | Warnings and informational notices |

### Session / onboarding cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| Session header cells | Card with border | `history_cell/session.rs` | Welcome header, onboarding guidance, session info |
| Image-related cells | Image preview | `history_cell/patches.rs` | View-image and image-generation tool results |

## Base cell composites

Several reusable building blocks compose more complex cells:

| Struct | Purpose |
|---|---|
| `PlainHistoryCell` | Simple cell backed by static `Vec<Line>` |
| `PrefixedWrappedHistoryCell` | Text with configurable initial/subsequent line prefixes (e.g. `⚠ ` on first line, `  ` on wrapped lines) |
| `CompositeHistoryCell` | Combines multiple cells with blank-line separators |

All three implement `HistoryCell` and are used as building blocks inside the
concrete cell constructors.

## Bottom pane components

These live inside the ratatui viewport and are redrawn every frame. They're
closer to traditional UI components — stateful, interactive, and always visible.

The bottom pane has a **view stack** architecture: the `ChatComposer` is always
retained underneath, and transient `BottomPaneView`s (popups/modals) are pushed
on top. Views are dismissed on completion or cancellation.

### Core pane

| Component | Web analogy | File | Purpose |
|---|---|---|---|
| `BottomPane` | Layout container | `bottom_pane/mod.rs` | Owns composer + view stack, routes input, manages delayed approvals |
| `ChatComposer` | `<textarea>` with autocomplete | `bottom_pane/chat_composer.rs` | Multi-line input where you type prompts (with mentions, slash commands, history search) |
| `StatusIndicatorWidget` | Spinner / progress bar | `status_indicator_widget.rs` | Shows "Working…" spinner with interrupt hint while a task runs |

### Popup views

All implement the `BottomPaneView` trait (key handling, completion, rendering).

| Component | Web analogy | File | Purpose |
|---|---|---|---|
| `ApprovalOverlay` | Confirmation modal | `bottom_pane/approval_overlay.rs` | Approve/deny command execution and file writes |
| `RequestUserInputOverlay` | Form modal | `bottom_pane/request_user_input/` | Answer tool-requested questions |
| `ListSelectionView` | Command palette | `bottom_pane/list_selection_view.rs` | Generic list picker (models, settings, skills, etc.) |
| `SkillPopup` | Autocomplete dropdown | `bottom_pane/skill_popup.rs` | Skill selection when typing `$` |
| `CommandPopup` | Slash command picker | `bottom_pane/command_popup.rs` | `/` command autocomplete |
| `FileSearchPopup` | Fuzzy file finder | `bottom_pane/file_search_popup.rs` | File search results |
| `MentionsV2Popup` | @-mention autocomplete | `bottom_pane/mentions_v2/` | Plugin/connector mention picker |
| `FeedbackView` | Rating modal | `bottom_pane/feedback_view.rs` | Feedback collection after responses |
| `HooksBrowserView` | Settings list | `bottom_pane/hooks_browser_view.rs` | Browse and configure hooks |
| `MemoriesSettingsView` | Toggle list | `bottom_pane/memories_settings_view.rs` | Memory/notes management |
| `SkillsToggleView` | Toggle list | `bottom_pane/skills_toggle_view.rs` | Enable/disable skills |
| `ExperimentalFeaturesView` | Toggle list | `bottom_pane/experimental_features_view.rs` | Toggle experimental features |
| `StatusLineSetupView` | Multi-select picker | `bottom_pane/status_line_setup.rs` | Configure status bar items |
| `TerminalTitleSetupView` | Multi-select picker | `bottom_pane/title_setup.rs` | Configure terminal title items |
| `AppLinkView` | External link card | `bottom_pane/app_link_view.rs` | MCP server app install/enable flow |
| `McpServerElicitationOverlay` | Form modal | `bottom_pane/mcp_server_elicitation.rs` | MCP server configuration forms |
| `PendingInputPreview` | Queue sidebar | `bottom_pane/pending_input_preview.rs` | Shows queued prompts and pending steers |
| `PendingThreadApprovals` | Notification chip | `bottom_pane/pending_thread_approvals.rs` | Inactive threads needing approval |
| `UnifiedExecFooter` | Process summary | `bottom_pane/unified_exec_footer.rs` | Background process status summary |

## Overlay screens

Full-screen UIs rendered on the **alternate screen**. These take over the
entire terminal and return to the inline chat when dismissed.

| Overlay | Web analogy | File | Purpose |
|---|---|---|---|
| `TranscriptOverlay` | Full-page scroll view | `pager_overlay.rs` | `Ctrl+T` — full conversation transcript with cached live tail |
| `StaticOverlay` | Static page | `pager_overlay.rs` | Generic full-screen content display |
| `OnboardingScreen` | Multi-step wizard | `onboarding/onboarding_screen.rs` | First-run experience (welcome → auth → trust directory) |
| `ResumePicker` | Fuzzy finder | `resume_picker.rs` | Session resume picker with transcript preview |
| Diff viewer | Side-by-side diff | `diff_render.rs` | File diff viewing with syntax highlighting |

The transcript overlay deserves special attention: it renders committed cells
plus a **cached live tail** from the in-flight active cell. The cache key is
derived from terminal width, an active-cell revision counter, a
stream-continuation flag, and an animation tick. This avoids expensive
rebuilding of wrapped `Line`s on every frame while keeping in-flight tool calls
visible.

## Layout system

Codex uses a custom flex-box-style layout system defined in
`render/renderable.rs`:

| Layout widget | Web analogy | Purpose |
|---|---|---|
| `FlexRenderable` | CSS flexbox column | Distributes vertical space; flex > 0 children share remaining space |
| `ColumnRenderable` | Vertical `Stack` | Renders children top-to-bottom |
| `RowRenderable` | Horizontal `Row` | Renders children left-to-right with fixed widths |
| `InsetRenderable` | CSS padding | Applies insets (top/left/bottom/right) to a child |

The main chat surface composes as:
```
FlexRenderable (column)
  ├── active_cell        (flex=1, fills remaining space)
  ├── active_hook_cell   (flex=0, fixed height)
  └── bottom_pane        (flex=0, fixed height, with 1-row top inset)
```

## Event flow

```
App Server (codex-app-server)
  │
  ├── sends AppServerEvent ──→ mpsc channel ──→ App event loop
  │                                                  │
  │                                         App::handle_event()
  │                                              (event_dispatch.rs)
  │                                                  │
  │                                    ┌─────────────┴─────────────┐
  │                                    │                           │
  │                              ChatWidget                App-level actions
  │                           (protocol event routing)    (pickers, config, etc.)
  │                                    │
  │                          ┌─────────┴──────────┐
  │                          │                    │
  │                   active_cell           finalize into cell
  │                   (in viewport)         (push to scrollback)
  │                                               │
  │                                      insert_history_lines()
  │                                      (escape sequences to
  │                                       terminal scrollback)
```

The TUI never calls the model or runs tools directly — it's a pure consumer of
events produced by the `AppServerSession`.

`AppEvent` is the internal message bus. Widgets emit events to request actions
that must be handled at the app layer (opening pickers, persisting config,
shutting down) without coupling directly to `App` internals.

## Streaming architecture

Streaming is managed through a layered controller system:

1. **`StreamState`** (`streaming/mod.rs`) — owns a `MarkdownStreamCollector`
   and a FIFO queue of committed render lines. All drains pop from the front.
2. **`StreamController`** (`streaming/controller.rs`) — adapts queued lines
   into `HistoryCell` emission rules for message and plan streams.
3. **`AdaptiveChunkingPolicy`** — computes drain rates based on queue pressure.
4. **`CommitTick`** (`streaming/commit_tick.rs`) — binds policy decisions to
   concrete controller drains.

Key invariant: queue ordering. All drains pop from the front, and enqueue
records an arrival timestamp so policy code can reason about oldest queued age.

When streaming completes, the controller **consolidates** transient
`AgentMessageCell`s into a single source-backed `AgentMarkdownCell` that can
re-render at any width for resize reflow.

## Input handling

Input routing is layered:

1. **`Tui`** reads crossterm key events and forwards them to `App`.
2. **`App`** routes to `ChatWidget::handle_key_event()`.
3. **`ChatWidget`** decides: slash command, interrupt, quit, or forward to
   `BottomPane`.
4. **`BottomPane`** routes: active view first, then composer.
   - Ctrl+C handling is layered: active views can consume it to dismiss
     themselves, then composer history search can cancel, then it clears draft
     input. `ChatWidget` owns the quit/interrupt state machine.
5. **`ChatComposer`** handles text editing, popups (slash, mentions, file
   search), and paste bursts.

The bottom pane supports **delayed approval prompts**: if the user is actively
typing, approval requests are queued and shown after an idle delay. This avoids
interrupting the composer mid-sentence.

## Key bindings

Key bindings are managed through a `RuntimeKeymap` system
(`keymap.rs`, `keymap_setup/`):

- **`RuntimeKeymap`** — resolved snapshot of all bindings, cloned into bottom
  pane surfaces after config reloads.
- **`PagerKeymap`** — bindings for overlay/pager views.
- **`ListKeymap`** — bindings for list selection views.
- **`ApprovalKeymap`** — bindings for approval overlays.
- Config persistence via `app/config_persistence.rs`.

## Keymaps support custom layouts** and the TUI can probe the terminal for
enhanced keyboard support (via CSI-u protocol).

## Styling and theming

Styling adapts to the terminal's color palette:

- **`style.rs`** — user message background blends with terminal bg (light:
  dark overlay; dark: light overlay). Accent color switches between cyan (dark)
  and a darker blue (light).
- **`terminal_palette.rs`** — probes terminal for ANSI color palette and
   picks the best approximation for intended colors.
- **`theme_picker.rs`** — theme selection UI.
- **`color.rs`** — color blending utilities for adapting to terminal
  backgrounds.

Cells use ratatui's `Style`, `Modifier`, and `Color` types. The system avoids
hardcoded colors where possible, preferring palette-aware color selection.

## Custom terminal

Codex uses a custom `Terminal` wrapper (`custom_terminal.rs`) derived from
ratatui's `Terminal` with key modifications:

- **Inline viewport** — tracks `viewport_area` and `last_known_cursor_pos`
  manually, since the inline mode doesn't get free cursor tracking from
  alternate-screen mode.
- **Viewport area management** — `set_viewport_area()` and
  `invalidate_viewport()` for precise control over what gets redrawn.
- **Scrollback integration** — methods for clearing scrollback, clearing after
  a position, and scroll-region manipulation.
- **Cursor style** — explicit `SetCursorStyle` support for blinking/steady
  bar/beam cursors.
- **Frame counting** — `visible_history_rows` tracks how many rows have been
  pushed above the viewport into native scrollback.

## Key source files

| Area | File(s) |
|---|---|
| Main event loop & dispatch | `app.rs`, `app/event_dispatch.rs` |
| Chat widget (cell manager) | `chatwidget.rs`, `chatwidget/*.rs` |
| Transcript state | `chatwidget/transcript.rs` |
| All cell types | `history_cell/*.rs` |
| Exec cell (command grouping) | `exec_cell/*.rs` |
| Bottom pane & composer | `bottom_pane/*.rs` |
| View trait | `bottom_pane/bottom_pane_view.rs` |
| Overlay screens (transcript, pager) | `pager_overlay.rs` |
| Session resume picker | `resume_picker.rs` |
| Onboarding flow | `onboarding/*.rs` |
| Layout system | `render/renderable.rs`, `render/mod.rs` |
| Streaming controllers | `streaming/*.rs` |
| Scrollback insertion | `insert_history.rs` |
| Markdown rendering | `markdown_render.rs`, `markdown_stream.rs` |
| Diff rendering | `diff_render.rs`, `diff_model.rs` |
| Style / theme | `style.rs`, `color.rs`, `theme_picker.rs`, `terminal_palette.rs` |
| Terminal setup | `custom_terminal.rs`, `tui.rs`, `terminal_probe.rs` |
| Key bindings | `keymap.rs`, `keymap_setup/*.rs`, `config/src/tui_keymap.rs` |
| App events | `app_event.rs` |
| App-level actions | `app/session_lifecycle.rs`, `app/thread_events.rs`, `app/thread_routing.rs` |
| Multi-agent support | `multi_agents.rs` |
| Ambient pets | `pets/*.rs` |
| Status display | `status/*.rs`, `status_indicator_widget.rs` |
| Token usage | `token_usage.rs` |
| Text wrapping | `wrapping.rs`, `live_wrap.rs`, `text_formatting.rs` |
| Clipboard | `clipboard_copy.rs`, `clipboard_paste.rs` |
| Notifications | `notifications/*.rs` |
| Transcript reflow | `transcript_reflow.rs`, `resize_reflow_cap.rs` |
| Terminal title | `terminal_title.rs` |
