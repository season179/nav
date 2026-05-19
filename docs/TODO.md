# TODO

## Must-Haves For Daily Coding

Minimum bar: nav should feel safe, resumable, and controllable for ordinary
repo work before adding speculative features. Ranked by importance.

1. [ ] Make permissions and execution safety first-class: command approval
   policy, dangerous-command gates, protected-file rules, external-directory
   detection, and a stronger sandbox story for shell execution. (in-progress permissions-execution-safety-nav)
   - Partial: workspace-boundary writes in `nav-core/src/tools/fs.rs`, bash
     timeout in `tools/shell.rs`. No approval policy, dangerous-command gates,
     protected-file rules, or sandbox yet.
2. [ ] Improve the editing and diff workflow: patch-style edits, multi-file
   mutation summaries, diff tracking, file references, and clearer "what
   changed" review affordances. ()
   - Partial: `edit_file` uses `old_str`/`new_str` string replacement
     (`nav-core/src/tools/mod.rs`). No patch-style edits, multi-file summaries,
     or diff tracking surfaced in the TUI.
3. [ ] Add real interactive control: abort the current turn, queue steering and
   follow-up messages while the agent is busy, and make `/help`, `/resume`, and
   `/sessions` real commands instead of popup labels.
   - Partial: slash labels rendered in `nav-tui/src/bottom_pane/slash_popup.rs`
     and a Ctrl+C handler exists, but the labels do not dispatch to real
     handlers and turn-abort / message queueing are not implemented.
4. [ ] Add reliability recovery: retry transient provider failures, handle
   context overflow, tune timeouts, and bound long tool output before it reaches
   the model/session log. (in-progress reliability-recovery-mechanisms)
   - Not started in code: only the bash tool has a timeout; no retry, no
     context-overflow handling, no tool-output bounding before the session log.
5. [x] Load project context and settings: discover `AGENTS.md` / `CLAUDE.md`,
   support `.nav/settings.json`, and show startup git/workspace status.
   - Done in commit 86d9e96: `AGENTS.md`/`CLAUDE.md` discovery at launch cwd
     and `~/.agents/` (`nav-core/src/project.rs`), `.nav/settings.json`
     loader feeding CLI defaults (`nav-core/src/cli.rs`), and git/workspace
     status surfaced in the TUI welcome cell, status bar, and NDJSON
     startup banner (`nav-tui/src/{cells,status_bar}.rs`).
6. [ ] Finish interactive session management: named sessions, resume picker,
   compaction, and exportable session transcripts.
   - Partial: SQLite session store at `$XDG_DATA_HOME/nav/nav.db` and
     `--resume <ULID>` replay work (`nav-core/src/agent/replay.rs`). No named
     sessions, resume picker UI, compaction, or transcript export.
7. [ ] Improve install, auth, and diagnostics UX: add `nav doctor`, clearer
   login/auth/model errors, and reliable update/reinstall checks.
   - Partial: contextual auth errors in `nav-core/src/auth.rs` and
     `nav update/upgrade` are implemented. No `nav doctor`.
8. [ ] Improve file ergonomics: support `@file` mentions, path completion,
   piped stdin, and paste handling. (in-progress improve-file-ergonomics-nav)
   - Mostly done: `@file` mentions + nucleo path completion
     (`nav-tui/src/bottom_pane/mention_popup.rs`), piped stdin
     (`nav-cli/src/main.rs`), and paste / clipboard-image handling
     (recent commits 50def85, 729ea70, d93b1a5, 262d0d4).
   - Outstanding: still tagged `(in-progress)` — recent commits are bug
     fixes (multibyte cursor panic, popup Down-arrow, escaped paths,
     resumed-session attachment paths), so polish work is ongoing.

## Good-To-Have After Daily Use

These should come after the must-haves unless a frontend or integration needs a
small slice earlier.

1. [ ] Add advanced session workflows: fork/clone, tree navigation, branch
   summaries, labels, and richer transcript search.
   - Not started.
2. [ ] Add optional git checkpointing: checkpoint/stash/restore support for
   users who want reversible agent turns.
   - Not started.
3. [ ] Deepen extensibility: custom tools, MCP-style integrations, extension
   hooks, prompt templates, package installation, and themes.
   - Partial: MCP integration referenced in `nav-tui/src/app.rs` and skills
     system in `nav-core/src/skills.rs`. No extension hooks, prompt templates,
     package install, or themes.
4. [ ] Polish headless integration: define a stable JSON/RPC contract for
   desktop, chat, and other non-TUI frontends.
   - Not started: NDJSON `AgentEvent` stream exists, but the wire format is
     not yet versioned or stabilized as a contract.
5. [ ] Add richer inputs after text flows are solid: file attachments, image
   attachments, and clipboard images.
   - Mostly done: image attachments and clipboard images shipped;
     `.nav/clipboard/` cache used by the TUI paste handler.
   - Outstanding: generic non-image file attachments (drag-and-drop or
     explicit attach beyond `@file` text inlining) are not implemented.
6. [ ] Revisit subagents only after the single-agent workflow is strong; Pi is
   daily-usable without them.
   - Not started.

## Provider API Adapters

- [x] Keep nav local-first. Do not depend on provider-side stored conversation
  state by default.
  - Done: `store: false` set in `nav-core/src/agent/runner.rs`; session
    persistence is local SQLite.
- [ ] Add a provider adapter boundary so `nav-core` works with nav's own
  normalized messages, tool calls, tool results, usage, and errors.
  - Partial: only the OpenAI Responses adapter exists
    (`nav-core/src/responses/mod.rs`). No pluggable trait separating provider
    wire formats from `nav-core`.
- [ ] Keep OpenAI-specific details inside the OpenAI adapter: Responses API
  input shape, `store: false`, encrypted reasoning content, and
  `function_call` / `function_call_output` items.
  - Partial: details are colocated under `nav-core/src/responses/` today, but
    without the adapter boundary (bullet above) there is no actual seam
    keeping them out of the rest of `nav-core`.
  - Outstanding: confirm encapsulation once a real adapter trait exists and
    a second adapter forces the seam.
- [ ] Define how Anthropic-style APIs map into the same internal shape:
  content blocks, `tool_use`, `tool_result`, and continuation state.
  - Not started.
- [ ] Define how completion-style APIs behave when they do not have native tool
  calling, either as a reduced mode or through a structured-output wrapper.
  - Not started.
- [ ] Persist only provider-neutral conversation state locally by default. If a
  provider needs opaque continuation data, store it locally with a provider name
  and version label.
  - Mostly done: persistence is local and provider-neutral by default.
  - Outstanding: opaque continuation data is not tagged with a provider
    name + version label; there is no schema for that today.
- [ ] Add recorded-fixture tests for each provider adapter before exposing it in
  the TUI.
  - Not started.
