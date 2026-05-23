# TUI Component Reference

A web-developer-friendly guide to nav's terminal UI architecture.

## Big picture

Nav's TUI is built on [ratatui](https://ratatui.rs/) (a Rust terminal UI
framework, similar in spirit to React for the terminal). It uses an **inline
viewport** instead of the alternate screen — meaning the app lives inside your
normal terminal session, and finalized output becomes part of your terminal's
native scrollback.

There are three layers, analogous to layout regions in a web app:

```
┌─────────────────────────────────────┐
│  Status bar          (fixed top)    │  ← always visible, 1 row
├─────────────────────────────────────┤
│                                     │
│  Native scrollback   (the "DOM")    │  ← terminal-owned, nav can't edit
│  - finalized cells                  │
│  - previous turns                   │
│                                     │
├─────────────────────────────────────┤
│  ratatui viewport                   │  ← nav-owned, redrawn each frame
│  - streaming assistant cell         │
│  - inflight tool call placeholders  │
├─────────────────────────────────────┤
│  Bottom pane        (fixed bottom)  │  ← always visible
│  - composer input                   │
│  - popups (slash, mentions, etc.)   │
└─────────────────────────────────────┘
```

## Key architectural difference from web

There is **no virtual DOM or reconciliation**. Cells are **write-once**:

1. An event arrives from the agent loop.
2. The `ChatWidget` translates it into a cell (a struct implementing
   `HistoryCell`).
3. Once finalized, the cell is rendered into styled terminal lines and written
   into the terminal's native scrollback via escape sequences
   (`insert_history_lines`).
4. The TUI never touches that content again.

The ratatui viewport only paints the **active streaming cell** and the **bottom
pane** — typically 5–15 rows total. This keeps rendering fast and memory
bounded regardless of session length.

## Cell types ("component library")

Cells are the atomic, finalized blocks in scrollback. Each is a Rust struct
that implements `HistoryCell` — think of it as a component with a single
`render()` method: `display_lines(width) -> Vec<Line>`.

### Conversation cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `UserMessageCell` | User chat bubble | `cells/messages.rs` | The prompt you typed, with attachments |
| `AssistantMessageCell` | Assistant chat bubble | `cells/messages.rs` | Streaming text from the model (finalized once done) |
| `SkillInvocationCell` | Collapsible callout | `cells/messages.rs` | Shows which skill was activated |

### Tool cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `ToolCallCell` | Collapsible `<details>` | `cells/tools.rs` | Tool name + args the model requested |
| `ToolOutputCell` | Collapsible result block | `cells/tools.rs` | Tool output shown back to the model |
| `ToolCallCell` (exploring group) | Collapsible group row | `cells/tools.rs` | Consecutive read-only tool calls (`Exploring (N calls)`) |

### Agent structure cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `SubagentCell` | Nested thread card | `cells/subagents.rs` | Sub-agent spawn / result |
| `CompactionCell` | Progress indicator | `cells/compaction.rs` | Context compaction phase display |
| `PendingInputCell` | Queued item card | `cells/pending.rs` | Queued prompts waiting their turn |
| `TurnAbortedCell` | Alert banner | `cells/pending.rs` | Turn was aborted by the user |

### Change / diff cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `FileChangeCell` | Diff view | `cells/changes.rs` | Individual file edits made by tools |
| `TurnDiffCell` | Summary diff | `cells/changes.rs` | Net changes for the whole turn |
| `GitCheckpointCell` | Badge / toast | `cells/changes.rs` | Git checkpoint / stash / restore notification |

### System / status cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `ErrorCell` | Error toast | `cells/system.rs` | Agent errors |
| `NoticeCell` | Info / warning toast | `cells/system.rs` | Warnings and informational notices |
| `ApprovalDecisionCell` | Inline prompt response | `cells/system.rs` | Approved / denied tool use |

### Session management cells

| Cell | Web analogy | File | Purpose |
|---|---|---|---|
| `ModelListCell` | Dropdown list | `cells/model.rs` | Output of `/model` |
| `ModelSetCell` | Confirmation toast | `cells/model.rs` | Output of `/model <name>` |
| `SessionListCell` | List view | `cells/sessions.rs` | `/sessions` output |
| `SessionTreeCell` | Tree view | `cells/sessions.rs` | `/tree` output |
| `SessionNoticeCell` | Status toast | `cells/sessions.rs` | Session rename, export, etc. |
| `TranscriptHitsCell` | Search results | `cells/sessions.rs` | `/find` matches |

## Bottom pane components

These live inside the ratatui viewport and are redrawn every frame. They're
closer to traditional UI components — stateful, interactive, and always visible.

| Component | Web analogy | File | Purpose |
|---|---|---|---|
| `Composer` | `<textarea>` with autocomplete | `bottom_pane/composer.rs` | Multi-line input where you type prompts |
| `SlashPopup` | Command palette dropdown | `bottom_pane/slash_popup.rs` | Autocomplete popup for `/` commands |
| `MentionPopup` | @-mention autocomplete | `bottom_pane/mention_popup.rs` | Autocomplete for skill mentions |
| `Approval` | Confirmation modal | `bottom_pane/approval.rs` | Approve/deny tool execution |
| `PendingPreview` | Queue preview sidebar | `bottom_pane/pending_preview.rs` | Shows queued prompts |
| `SessionPicker` | Fuzzy finder modal | `bottom_pane/session_picker.rs` | Resume-session picker (`/resume`) |

## Status bar

One fixed row at the top, rendered by `app/status_bar.rs`. Shows:

- Current model
- Working directory
- Git branch
- Status (*Ready* or *Working 5s*)

## Event flow

```
Agent loop (nav-core)
  │
  ├── emits AgentEvent ──→ mpsc channel ──→ TUI main loop
  │                                                │
  │                                         ChatWidget::ingest(event)
  │                                                │
  │                                    ┌───────────┴───────────┐
  │                                    │                       │
  │                              streaming cell          finalize into cell
  │                              (in viewport)           (push to scrollback)
  │                                                           │
  │                                                  insert_history_lines()
  │                                                  (escape sequences to
  │                                                   terminal scrollback)
```

The TUI never calls the model or runs tools directly — it's a pure consumer of
`AgentEvent`s produced by `nav-core`.

## Key source files

| Area | File(s) |
|---|---|
| Main event loop & layout | `app/mod.rs`, `app/render.rs` |
| Chat widget (cell manager) | `widget.rs` |
| All cell types | `cells/*.rs` |
| Bottom pane components | `bottom_pane/*.rs` |
| Input parsing & slash commands | `input/commands.rs`, `input/mod.rs` |
| Status bar | `app/status_bar.rs` |
| Scrollback insertion | `insert_history.rs` |
| Streaming text handling | `streaming.rs` |
| Theme / colors | `theme.rs` |
| Terminal setup | `app/terminal.rs`, `custom_terminal.rs` |
| Turn lifecycle | `app/turn_lifecycle.rs`, `app/turn_task.rs` |
| Session management | `app/session.rs` |
