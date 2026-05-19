# TODO

## Must-Haves For Daily Coding

Minimum bar: nav should feel safe, resumable, and controllable for ordinary
repo work before adding speculative features. The current daily-driver blockers
are the unchecked items at the top; shipped foundation stays here as evidence.

1. [ ] Add real interactive control: abort the current turn/tool, queue
   steering and follow-up messages while the agent is busy, and surface a
   visible/editable pending queue.
   - Partial: slash labels render in `nav-tui/src/bottom_pane/slash_popup.rs`
     and a Ctrl+C handler exists, but prompts submitted during an active turn
     still return "agent is busy" in `nav-tui/src/app.rs`; Ctrl+C only counts
     toward quitting; no turn-abort, running-tool abort, steering, or
     follow-up queue is implemented.
   - Reference shape: Codex has an input queue / interrupt path; Pi supports
     streaming steering, queued follow-ups, and abort keybindings.
2. [ ] Stream assistant output live in the TUI.
   - Partial: `AssistantMessageDelta` events exist and streaming message cells
     exist, but `nav-tui/src/widget.rs` explicitly ignores deltas and only
     renders final `AssistantMessageDone` text.
3. [ ] Finish interactive session management: TUI resume picker, real
   `/sessions` and `/resume` commands, named sessions, and exportable
   transcripts.
   - Partial: SQLite session store at `$XDG_DATA_HOME/nav/nav.db`,
     `--resume <ULID>`, and `--list-sessions` exist. No TUI resume picker,
     session naming command, or transcript export.
4. [ ] Add long-session compaction: manual `/compact`, automatic threshold
   compaction, persisted summaries, and clear replay behavior after compaction.
   - Partial: context-window overflow recovery can drop the oldest tool pair
     and retry, but there is no user-visible compaction flow. Resume replay is
     text-only and intentionally skips old tool-call events in
     `nav-core/src/agent/replay.rs`.
5. [ ] Improve install, auth, and diagnostics UX: add `nav doctor`, clearer
   login/auth/model errors, and reliable update/reinstall checks.
   - Partial: contextual auth errors in `nav-core/src/auth.rs` and
     `nav update/upgrade` are implemented. No `nav doctor`.
6. [x] Make permissions and execution safety first-class: command approval
   policy, dangerous-command gates, protected-file rules, external-directory
   detection, and a stronger sandbox story for shell execution.
   - Done in commit f623e52: `bash`/`edit_file` preflight pipeline under
     `nav-core/src/tools/preflight.rs`; command classifier, dangerous-command
     gates, safe-command allowlist, and bash AST parser
     (`nav-core/src/permissions/{classifier,dangerous,safe_commands,bash_parse}.rs`);
     protected-path rules for `.git`/`.agents`/`.nav` plus gated reads of
     `.env*`/`*.pem`/`*.key`/SSH keys (`permissions/protected.rs`);
     external-directory detection (`permissions/external.rs`); macOS Seatbelt
     sandbox with embedded `.sbpl` profile and Linux/Windows passthrough
     (`nav-core/src/sandbox/{mod,seatbelt,passthrough}.rs`); approval flow via
     `ChannelGate` with TUI prompt cell (`nav-tui/src/bottom_pane/approval.rs`)
     and NDJSON reverse channel; persisted `approval` audit table
     (`nav-core/src/session/init.sql`); CLI flags `--approval-policy`,
     `--sandbox`, `--dangerously-bypass-approvals-and-sandbox`.
7. [x] Improve the editing and diff workflow: patch-style edits, multi-file
   mutation summaries, diff tracking, file references, and clearer "what
   changed" review affordances.
   - Done in commit 2718a83: `apply_patch` tool with add/update/move/delete
     sections (`nav-core/src/tools/patch.rs`); per-call multi-file mutation
     summaries via the new `FileChange` `AgentEvent` and
     `mutation::FileChangeSummary` (`nav-core/src/mutation.rs`); turn-level
     unified-diff tracking via the `TurnDiff` event
     (`nav-core/src/git_diff.rs`); new TUI cells render file-change /
     turn-diff review output (`nav-tui/src/cells.rs`).
8. [x] Add reliability recovery: retry transient provider failures, handle
   context overflow, tune timeouts, and bound long tool output before it reaches
   the model/session log.
   - Done in commit 0b5624a (with follow-ups b4cc4c0, 9ff191f, 9b49327):
     exponential-backoff retry with jitter + `Retry-After` in
     `nav-core/src/responses/retry.rs`; one-shot context-window recovery
     that drops the oldest tool pair and retries; SSE/WS idle timeouts
     plus a new `--idle-timeout-secs` flag and reqwest `connect_timeout`
     + `pool_idle_timeout`; tool-output truncation in
     `nav-core/src/tools/truncate.rs` (50KB / 2000 lines, head-only for
     `read_file`/`code_search`, head+tail for bash); new durable
     `ProviderRetry` and `ContextTrimmed` `AgentEvent` variants.
9. [x] Load project context and settings: discover `AGENTS.md` / `CLAUDE.md`,
   support `.nav/settings.json`, and show startup git/workspace status.
   - Done in commit 86d9e96: `AGENTS.md`/`CLAUDE.md` discovery at launch cwd
     and `~/.agents/` (`nav-core/src/project.rs`), `.nav/settings.json`
     loader feeding CLI defaults (`nav-core/src/cli.rs`), and git/workspace
     status surfaced in the TUI welcome cell, status bar, and NDJSON
     startup banner (`nav-tui/src/{cells,status_bar}.rs`).

## Good-To-Have After Daily Use

These should come after the must-haves unless a frontend or integration needs a
small slice earlier.

1. [ ] Finish file ergonomics polish after real dogfooding: `@file` mentions,
   path completion, piped stdin, paste handling, and generic attachments.
   - Mostly done: `@file` mentions + nucleo path completion
     (`nav-tui/src/bottom_pane/mention_popup.rs`), piped stdin
     (`nav-cli/src/main.rs`), and paste / clipboard-image handling
     (recent commits 50def85, 729ea70, d93b1a5, 262d0d4).
   - Outstanding: recent work is bug-fix polish (multibyte cursor panic,
     popup Down-arrow, escaped paths, resumed-session attachment paths) and
     generic non-image file attachments are not implemented.
2. [ ] Add advanced session workflows: fork/clone, tree navigation, branch
   summaries, labels, and richer transcript search.
   - Not started.
3. [ ] Add optional git checkpointing: checkpoint/stash/restore support for
   users who want reversible agent turns.
   - Not started.
4. [ ] Deepen extensibility: custom tools, MCP-style integrations, extension
   hooks, prompt templates, package installation, and themes.
   - Partial: MCP integration referenced in `nav-tui/src/app.rs` and skills
     system in `nav-core/src/skills.rs`. No extension hooks, prompt templates,
     package install, or themes.
5. [ ] Polish headless integration: define a stable JSON/RPC contract for
   desktop, chat, and other non-TUI frontends.
   - Not started: NDJSON `AgentEvent` stream exists, but the wire format is
     not yet versioned or stabilized as a contract.
6. [ ] Revisit subagents only after the single-agent workflow is strong; Pi is
   daily-usable without them.
   - Not started.

## Provider API Adapters

Provider neutrality is no longer a personal daily-driver must-have while nav is
primarily an OpenAI/Codex-backed local agent. Keep this as a later architecture
track unless multi-provider daily use becomes a real requirement.

- [x] Keep nav local-first. Do not depend on provider-side stored conversation
  state by default.
  - Done: `store: false` set in `nav-core/src/responses/request.rs`; session
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
