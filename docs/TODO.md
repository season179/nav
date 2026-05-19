# TODO

## Must-Haves For Daily Coding

Minimum bar: nav should feel safe, resumable, and controllable for ordinary
repo work before adding speculative features. The current daily-driver blockers
are the unchecked items at the top; shipped foundation stays here as evidence.

1. [x] Add real interactive control: abort the current turn/tool, queue
   steering and follow-up messages while the agent is busy, and surface a
   visible/editable pending queue.
   - Done in worktree branch `codex/main-worktree-20260519`: `ControlPlane`
     serializes active turns and pending inputs; active TUI submissions become
     queued follow-ups or `/steer` messages; `/abort` and Ctrl+C abort the
     active turn and pending approvals; `/queue-edit`, `/queue-remove`, and
     `/queue-clear` update the visible pending queue. The runner drains
     steering at the next safe model/tool boundary and skips stale tool calls
     when steering is injected. Durable `AgentEvent` variants record queued,
     edited, removed, cleared, dequeued, and aborted control actions.
   - Verified with `cargo fmt --check` and
     `cargo test -p nav-core -p nav-tui` (458 nav-core tests, 69 nav-tui unit
     tests, 9 composer tests, 18 snapshot tests, plus doc-tests).
2. [x] Stream assistant output live in the TUI.
   - Done in commit 9c55788: `ChatWidget` now holds a `streaming_assistant:
     Option<AssistantMessageCell>` opened on the first
     `AssistantMessageDelta`, appended on every subsequent delta, and
     finalized into scrollback on `AssistantMessageDone`. A new
     `close_streaming_assistant()` flushes the in-flight cell on
     `TurnComplete`, `TurnAborted`, any tool-call event
     (`ToolCallStarted` / `ToolCallOutput` / `ToolCallApprovalRequest` /
     `ToolCallBlocked` / `FileChange` / `TurnDiff`), `UserMessage`,
     `PendingInput*`, `Compaction*`, `ProviderRetry`, `ContextTrimmed`, and
     `Error` so a later assistant message starts a fresh row. Mid-stream
     `TurnAborted` preserves the partial text. Resume/replay still works
     because deltas are not persisted (`replay.rs:87`,
     `session/mod.rs:232,373`): only `AssistantMessageDone` fires and the
     non-streaming `AssistantMessageCell::new(text)` path still paints the
     full text. The render hot path reuses the existing
     `StreamController::partitioned_lines` helper, so scrollback redraws
     stay smooth.
   - Verified with `cargo fmt --check` and `cargo test -p nav-core -p
     nav-tui` (558 tests across 6 suites). New unit tests in
     `crates/nav-tui/tests/snapshot.rs` cover incremental painting, the
     resume-style `Done`-without-deltas path, mid-stream tool-call
     interleaving (two separate assistant rows in chronological order), and
     mid-stream `TurnAborted` preserving the partial text.
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
5. [x] Improve install, auth, and diagnostics UX: add `nav doctor`, clearer
   login/auth/model errors, and reliable update/reinstall checks.
   - Done in commit 5cb6bb3: `nav doctor` subcommand with runtime, auth,
     storage, project, and install check groups
     (`nav-core/src/doctor.rs`, `nav-core/src/cli.rs:121`,
     `nav-cli/src/main.rs:177`); each row formatted as
     `[ok]/[warn]/[fail] label — detail` with grouped headers and a
     `--json` variant; exit code flips to 1 when any row fails.
     Action-first auth errors in `nav-core/src/auth.rs` for missing
     `OPENAI_API_KEY`, missing/parse-failing `auth.json`, and wrong
     `auth_mode`. Model typo guard (`nav-core/src/models.rs`) plus
     `did_you_mean` enrichment of provider 4xx bodies in
     `nav-core/src/responses/{mod,sse,websocket}.rs`. `nav update`
     reliability: manifest-dir pre-check, version diff via
     `doctor::binary_version`, and PATH-shim warning against
     `cargo_install_bin_dir` (`nav-cli/src/main.rs:run_upgrade`).
   - Verified with `cargo fmt --all --check` and `cargo test -p
     nav-core -p nav-cli` (484 tests). Manual: `nav doctor` on healthy
     install (exit 0); `env -i HOME=$HOME PATH=/usr/bin:/bin nav
     --auth api-key doctor` flips to exit 1 with `[fail]` on `rg`
     (missing) and `credential` (missing key); `nav doctor` in `/tmp`
     reports `not a git repository` and no context files; `nav doctor
     --json` produces a parseable single object. Out of scope:
     network reachability, TUI doctor panel, release-feed
     auto-update. Connects to items 1, 3, 4, 6, 7, 8, 9 as the
     diagnostics layer over the work they shipped.
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

1. [x] Finish file ergonomics polish after real dogfooding: `@file` mentions,
   path completion, piped stdin, paste handling, and generic attachments.
   - Done in commit cae8524: `UserAttachment::File` variant carrying a
     workspace-relative path (`nav-core/src/agent/events.rs`); shared
     `resolve_workspace_path` + UTF-8-only `load_file_attachment` in
     `nav-core/src/agent/runner.rs` emit a fenced `input_text` part, bounded
     by the same 50 KB / 2000-line head-only truncation as `read_file`
     output (`nav-core/src/tools/truncate.rs`); protected-read attachments
     (`.env*`, `*.pem`, `*.key`, SSH keys) route through the existing
     approval gate via a new `attachment_read` tool name with the
     `protected_read` reason, including the steering path; mention popup
     queues images vs. files by extension and surfaces both via
     `push_pending_image` / `push_pending_file` on `Composer`, threaded
     through `ComposerEvent::Submit`, `AppEvent::Submit`, and `pending_draft`
     so the chip + reconciliation logic shipped for images applies to files
     unchanged; session resume rebuilds `File` rows through the same
     `build_user_content` path (`#[serde(tag = "kind")]` keeps old
     image-only logs deserializing).
   - Verified with `cargo fmt --check`, `cargo test -p nav-core -p nav-tui`
     (467 nav-core tests, 90 nav-tui tests across 4 suites, 0 failures),
     and `cargo clippy --all-targets` (1 pre-existing warning unrelated to
     this change).
   - Deferred outside this checklist item: PDF/Word/binary extraction
     (UTF-8 only for v1), drag-and-drop from outside the workspace, a
     dedicated `/attach` slash command. Earlier bug-fix polish (multibyte
     cursor panic, popup Down-arrow, escaped paths, resumed-session
     attachment paths) shipped in commits 50def85, 729ea70, d93b1a5,
     262d0d4 before this work.
2. [x] Add advanced session workflows: fork/clone, tree navigation, branch
   summaries, labels, and richer transcript search.
   - Done in commit dc6cfd0: schema v3 migration adds
     `session.parent_id`/`fork_point_seq`, a `label` table with
     `idx_label_name`, and an `event_fts` FTS5 virtual table backed by
     insert/delete triggers that extract `$.text` from user/assistant
     event payloads (`nav-core/src/session/init.sql`,
     `nav-core/src/session/mod.rs`). `SessionStore` gains
     `fork_session`, `list_children`, `walk_tree`, `add_label`,
     `remove_label`, `labels_for`, `list_by_label`, and
     `search_transcript`, and `SessionSummary` carries `parent_id`,
     `labels`, and `child_count` for the picker. The CLI exposes a new
     `nav sessions {fork,tree,label,unlabel,search}` subcommand group;
     `--list-sessions` keeps working and now indents children under
     their parent and shows labels. TUI mirrors the surface with
     `/fork [seq]`, `/tree`, `/label <text>`, `/unlabel <text>`, and
     `/find <query>`; the `/sessions` cell auto-switches to tree mode
     when any `parent_id` is present in the result set. Fork resume
     reads its own event stream because parent events are copied at
     fork time.
   - Verified with `cargo fmt --all -- --check`, `cargo clippy
     -p nav-core -p nav-cli -p nav-tui --all-targets`, and `cargo test
     -p nav-core -p nav-cli -p nav-tui` (564 tests across 7 suites,
     including the new v2→v3 migration, fork-copies-events,
     labels-round-trip, depth-ordered `walk_tree`, and
     cross-session FTS phrase coverage).
3. [ ] Add optional git checkpointing: checkpoint/stash/restore support for
   users who want reversible agent turns.
   - Not started.
4. [ ] Deepen extensibility: custom tools, MCP-style integrations, extension
   hooks, prompt templates, package installation, and themes.
   - Partial: skills system in `nav-core/src/skills.rs` provides
     project/user-scope skill discovery and execution.
   - Outstanding: MCP-style integrations (no client, transport, or tool
     bridge yet), extension hooks, prompt templates, package install, and
     themes.
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
