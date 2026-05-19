# TODO

## Must-Haves For Daily Coding

Minimum bar: nav should feel safe, resumable, and controllable for ordinary
repo work before adding speculative features. The current daily-driver blockers
are the unchecked items at the top; shipped foundation stays here as evidence.

1. [ ] Add real interactive control: abort the current turn/tool, queue
   steering and follow-up messages while the agent is busy, and surface a
   visible/editable pending queue.
   - Partial: follow-up queueing slice done. Prompts submitted during an
     active turn now flow into a `PendingQueue`
     (`nav-tui/src/pending_input.rs`) instead of the old "agent is busy"
     error. The queue is rendered above the composer
     (`nav-tui/src/pending_queue_widget.rs`), drains FIFO when the active
     turn settles (`drain_next_queued` in `nav-tui/src/app.rs`), preserves
     each item's attachments and snapshotted slash-skill activation, and
     supports Ctrl+E (edit most-recent) and Ctrl+X (clear queue). `/clear`
     also clears the queue. Tested by unit tests in `pending_input.rs` and
     `pending_queue_widget.rs` plus app-level helpers
     (`enqueue_busy_submit`, `restore_for_edit`).
   - Partial: turn/tool abort done. Esc while a turn is active trips an
     `AbortSignal` (`nav-core/src/agent/abort.rs`) plumbed through
     `PermissionContext` into the agent loop and `SandboxRequest`. The
     runner checks the signal at five boundaries — between turns, during
     stream consumption, post-response pre-dispatch, between tool
     dispatches, and after the tool loop — kills any in-flight bash
     child via `tokio::select!` against `abort.wait()`, races
     `transport.create()` so a stuck connect can't outlive Esc, and
     emits a durable `AgentEvent::TurnAborted` (with a transcript-side
     `ToolAbortedCell` marker) in place of `TurnComplete`. All abort
     paths (Esc + approval-modal Abort decision) funnel through a
     unified `finalize_abort` that records the turn's tokens to the
     session store *before* emitting the durable event, and emits a
     `TurnDiff` when the working tree changed so reviewers see partial
     state regardless of which abort path fired. The status bar
     surfaces "Esc abort" while working. Tested by `agent/abort.rs`
     (including the trip-reason write/publish ordering test),
     `agent/tests.rs::{run_agent_emits_turn_aborted_when_signal_tripped_before_run,
     run_agent_skips_second_tool_when_abort_trips_during_first,
     run_agent_does_not_emit_turn_complete_when_last_tool_aborts}`,
     `sandbox/passthrough.rs::passthrough_aborts_long_running_command_quickly`,
     and `app.rs::{abort_key_only_fires_on_bare_esc,
     pressing_abort_key_trips_the_active_signal}`.
   - Partial: mid-turn steering done. `/steer <message>` during an
     active turn pushes into a `SteeringQueue`
     (`nav-core/src/agent/steering.rs`) clone shared with the runner
     via `PermissionContext`. The agent loop drains the queue at the
     top of the `'turns` loop before each model request, and if a
     final response arrives with no tool calls but pending steering is
     present, it appends the assistant response into `input`, folds
     the steering in atomically, records the turn's tokens, and
     re-enters `'turns` instead of dropping the user's nudge. With no
     active turn `/steer` degrades to a normal Submit. Pending
     steering is shown as its own row in the pending queue widget
     (count + "injects at next model/tool boundary"). On terminal
     turn events the TUI rescues any steering message that landed in
     the post-final-drain race window by converting it into a
     follow-up submit. Ctrl+X and `/clear` both drain the steering
     queue alongside the follow-up queue. The slash command parser is
     `parse_steer_command` in `nav-tui/src/input.rs`. Tested by
     `agent/steering.rs` unit tests (push/drain order, drop-oldest cap,
     atomic-counter consistency under concurrent submit/drain),
     `agent/tests.rs::run_agent_drains_steering_into_input_before_each_request`,
     `pending_queue_widget` (`empty_follow_up_queue_still_renders_steering_pending_row`,
     `zero_steering_omits_steering_row`), and
     `input.rs` (`parse_steer_command_returns_payload_when_followed_by_whitespace`,
     `parse_steer_command_handles_bare_command_and_unrelated_prefixes`).
   - Outstanding: queue state is not yet mirrored into the
     `AgentEvent` stream, so non-TUI consumers can't see follow-up or
     steering queue updates as structured events; per-item follow-up
     removal beyond "edit last" / "clear all" is not wired (no focus
     model in the queue widget today); abort doesn't currently clear
     in-flight assistant deltas from the transcript (the
     `AssistantMessageDone` event may still arrive after a
     `TurnAborted` in races between provider streaming and the abort
     check — story 14); `kill_on_drop` does not reach grandchildren of
     `sh -c` pipelines (would need `setsid`/process-group setup).
   - Reference shape: Codex has an input queue / interrupt path; Pi supports
     streaming steering, queued follow-ups, and abort keybindings.
2. [ ] Stream assistant output live in the TUI.
   - Partial: `AssistantMessageDelta` events exist and streaming message cells
     exist, but `nav-tui/src/widget.rs` explicitly ignores deltas and only
     renders final `AssistantMessageDone` text.
3. [x] Finish interactive session management: TUI resume picker, real
   `/sessions` and `/resume` commands, named sessions, and exportable
   transcripts.
   - Done in commit 1b9278c: bottom-pane TUI resume picker via
     `--pick-session` and bare `/resume`; `/sessions` cell backed by the same
     session summary query as `--list-sessions`; `/resume <ulid-or-prefix>`
     with unique-prefix resolution, ambiguous-prefix errors, and mid-turn
     refusal; nullable non-unique session names via v2 SQLite migration,
     `--name`, and `/name <text>`; transcript export via `/export [path]` and
     `nav export <ulid> [--format md|json] [--out path]` with extension
     inference, markdown sections/details, and JSON `AgentEvent` arrays.
   - Verified with `cargo fmt --all`, `cargo test -p nav-core -p nav-tui`,
     and `cargo test -p nav-cli`. Caveat: `cargo insta review` was unavailable
     in this environment (`cargo` had no `insta` subcommand), so the generated
     session-management cell snapshot was inspected and accepted manually.
4. [x] Add long-session compaction: manual `/compact`, automatic threshold
   compaction, persisted summaries, and clear replay behavior after compaction.
   - Done in commit 0a86743 (merged as 82484e1): first-class compaction module
     with Codex-style checkpoint prompt + replacement-history builder; durable
     `AgentEvent::Compaction{Started,Completed,Failed}` events; non-steerable
     manual `/compact`; persisted SQLite checkpoints; replay slicing from the
     latest checkpoint; TUI `/compact` routing, compaction cells, and queued
     prompts while compaction is running; pre-turn automatic compaction via
     `auto_compact_fraction × auto_compact_token_limit` CLI/settings; overflow
     trim fallback for normal and compaction turns.
   - Verified with `cargo test -p nav-core -p nav-tui` (438 nav-core tests, 63
     nav-tui unit tests, 7 composer tests, 16 snapshot tests). Deferred outside
     this checklist item: true mid-turn compaction inside an active tool loop;
     current automatic compaction runs before the next user turn.
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
