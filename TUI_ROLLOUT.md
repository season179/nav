# TUI Rollout — `/goal` prompts

Six slices that walk nav from "one-shot CLI" to "codex-style interactive TUI
with persistent SQLite-backed sessions." Each slice is a separate `/goal`
invocation with mechanical acceptance the Haiku evaluator can read directly
out of the transcript.

The schema baked in by slice A.5 is forward-compatible with multi-provider
support (OpenRouter, Deepseek, Z.ai, Anthropic, etc.) without requiring a
migration. The provider-trait extraction itself is a later slice, sketched at
the bottom under "After E".

## Dependency graph

```
A (AgentEvent + run_agent + TurnUsage)
        │
        ├──────► A.5 (SQLite session storage)   ─┐
        │                                         │
        └──────► B (nav-tui crate + chat widget) │
                       │                          │
                       ├──────► C (streaming)    │
                       │                          │
                       └──────► D (composer)     │
                                                  ▼
                                          E (wire nav binary)
```

- **A → everything**: nothing else makes sense without `AgentEvent` and the
  `TurnUsage` shape.
- **A.5 ∥ B**: parallelizable in separate worktrees. A.5 lives in
  `nav-core` + `nav-cli`; B lives in a new `nav-tui` crate. The only conflict
  surface is workspace `Cargo.toml` and `Cargo.lock`.
- **B → C, B → D**.
- **C ∥ D**: parallelizable in separate worktrees. They touch disjoint
  modules (`streaming/` vs `bottom_pane/`) and only collide trivially in
  `nav-tui/src/lib.rs` and `nav-tui/Cargo.toml`.
- **E**: must come last. It composes the TUI with the session store.

### Parallel-run instructions

After A lands on `main`, A.5 and B can run in parallel:

```sh
cd /Users/season/Personal/nav
git worktree add ../nav-slice-a5 -b slice-a5-storage
git worktree add ../nav-slice-b  -b slice-b-tui
# run slice A.5 in one Claude Code session at ../nav-slice-a5
# run slice B  in another at ../nav-slice-b
# when both are green, merge slice-a5-storage then slice-b-tui to main
# expect trivial conflicts on workspace Cargo.toml / Cargo.lock
git worktree remove ../nav-slice-a5
git worktree remove ../nav-slice-b
```

After B is merged, C and D can run in parallel the same way:

```sh
git worktree add ../nav-slice-c -b slice-c-streaming
git worktree add ../nav-slice-d -b slice-d-composer
# run slice C and D in two Claude Code sessions
# merge slice-c-streaming then slice-d-composer
git worktree remove ../nav-slice-c
git worktree remove ../nav-slice-d
```

If you hit a non-trivial conflict, that's a signal one slice grew beyond its
scope — revisit the constraints rather than papering over.

---

## How to run each slice

In a fresh Claude Code session at the right working directory (nav root, or
the worktree for parallelized slices):

1. Paste the entire `/goal …` block from the slice below. The first line
   must start with `/goal ` — everything after that line is part of the
   condition.
2. Make sure auto mode is on so Claude doesn't stall on per-tool prompts.
3. Let it run. The evaluator decides completion per turn.

Each slice caps itself at a turn limit and refuses to silently loop on
failing tests.

---

## Slice A — AgentEvent foundation

```
/goal Land the AgentEvent foundation in nav-core so a TUI and a session store can consume it.

End state — all of these must be demonstrably true in the transcript:

1. `cargo build --workspace` exits 0.
2. `cargo test --workspace` exits 0.
3. `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
4. `cargo fmt --check` exits 0.
5. `nav-core` exports a public `AgentEvent` enum with at least these
   variants:
     - AssistantMessageDelta { text }               -- transient stream chunk
     - AssistantMessageDone { text }                -- final assistant text
     - ToolCallStarted { call_id, name, arguments }
     - ToolCallOutput { call_id, output, is_error }
     - TurnComplete { usage: TurnUsage }
     - Error { message }
   Paste the final enum definition into the transcript.
6. `nav-core` exports a public `TurnUsage` struct with these four
   normalized u64 fields (all default 0):
     - tokens_input
     - tokens_output
     - tokens_input_cached
     - tokens_reasoning
   Providers that don't report a field leave it 0. Paste the struct.
7. `nav-core` exports `pub async fn run_agent(...)` that drives the
   agent loop and emits AgentEvent through a channel or stream.
   It emits AssistantMessageDelta per streaming chunk AND emits
   AssistantMessageDone once with the coalesced final text. It
   emits TurnComplete with usage populated from the OpenAI Responses
   `usage` object: input_tokens, output_tokens,
   input_tokens_details.cached_tokens, output_tokens_details.
   reasoning_tokens. Missing fields default to 0. Paste the
   signature and the usage-mapping code.
8. `nav-cli/src/main.rs` no longer contains the agent loop body — it
   calls `nav_core::run_agent` and renders the event stream to stderr
   so observable CLI output for an existing prompt is unchanged.
   Run the same prompt before and after; paste both outputs.
9. A unit test in `nav-core` proves: given a stub Responses transport
   returning one tool call followed by a final assistant message with
   usage { input_tokens: 100, output_tokens: 50, cached_tokens: 20 },
   `run_agent` emits the expected AgentEvent sequence in order,
   ending with TurnComplete carrying usage.tokens_input=100,
   tokens_output=50, tokens_input_cached=20, tokens_reasoning=0.
   Paste the test source and `cargo test -p nav-core` output.

Constraints — failing any of these means the goal is NOT met:
- Do not add a TUI. Do not add a `nav-tui` crate. Do not add ratatui,
  crossterm, or any TUI dependency.
- Do not add a SQLite store yet (that is slice A.5). Do not add
  rusqlite as a dep.
- Do not extract a ModelProvider trait yet — the OpenAI Responses
  code stays in its current module shape.
- Do not modify `nav-desktop/`, `ui/`, or the Electron preload bridge.
- Do not weaken or delete existing tests.
- Do not introduce `#[allow]` attributes to silence clippy.

If after 40 turns the end state is not satisfied, stop and report which
item is blocking with the exact command output. Do not loop on a
failing test — diagnose root cause and fix it, or stop.
```

---

## Slice A.5 — SQLite session storage

**Run in a worktree** (`../nav-slice-a5`) if running in parallel with B.

```
/goal Add SQLite-backed session storage to nav-core, with --resume and --list-sessions flags on nav-cli.

End state — all of these must be demonstrably true in the transcript:

1. `cargo build/test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` all exit 0.
2. `nav-core` adds deps `rusqlite` (with `bundled`), `ulid`, `dirs`. Paste Cargo.toml lines.
3. DB path: `dirs::state_dir()` on Linux, `data_local_dir()` elsewhere, joined with `nav/nav.db`. Parent created if missing. Paste resolver.
4. On open: `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`. PRAGMA values echoed at startup.
5. Schema applied as a single `init.sql` migration recorded in `schema_version(version INTEGER PK, applied_at INTEGER NN)`. NN = NOT NULL; all INTEGER count columns default 0. Paste the actual CREATE TABLE SQL from the migration file. Tables:
   - `session`: id TEXT PK (ULID); cwd/provider/model TEXT NN; title/profile TEXT; provider_meta TEXT (JSON); status TEXT NN default 'active'; cost_currency TEXT NN default 'USD'; created_at/updated_at INTEGER NN; INTEGER NN: tokens_input, tokens_output, tokens_input_cached, tokens_reasoning, cost_micros_reported, turns_with_reported_cost, turns_total. Indexes (cwd, updated_at DESC), (provider, updated_at DESC).
   - `event`: session_id TEXT NN REFERENCES session(id) ON DELETE CASCADE; seq/created_at INTEGER NN; kind/data TEXT NN (data = JSON AgentEvent); PK (session_id, seq).
   - `turn`: session_id TEXT NN REFERENCES session(id) ON DELETE CASCADE; turn_index/started_at INTEGER NN; ended_at INTEGER; model TEXT NN; INTEGER NN: tokens_input, tokens_output, tokens_input_cached, tokens_reasoning; cost_micros INTEGER NULL (NULL = unreported); cost_currency TEXT NN default 'USD'; cost_source TEXT NN default 'unreported' ('reported'|'unreported'); error TEXT; PK (session_id, turn_index).
6. `nav-core::SessionStore` public API (paste it): `open(Option<PathBuf>)`; `create_session(cwd, provider, model, profile) -> SessionId`; `append_event(session_id, &AgentEvent)` — durable variants only (skips AssistantMessageDelta), updates session.tokens_* from TurnComplete; `complete_turn(session_id, model, &TurnUsage, Option<ReportedCost>)` — inserts `turn`, rolls up session.cost_micros_reported / turns_with_reported_cost / turns_total; `load_session -> Vec<AgentEvent>`; `list_sessions(Option<&Path>) -> Vec<SessionSummary>` sorted updated_at DESC (SessionSummary has rollup fields).
7. `run_agent` accepts optional `&SessionStore` + SessionId and persists every durable AgentEvent. Paste the write path.
8. `nav-cli` gains `--resume <id>` and `--list-sessions [--cwd <path>]`. `--resume` loads events, rebuilds the Responses input transcript, continues. `--list-sessions` prints columns id, updated_at, cwd, model, tokens_total (input+output), cost formatted "$X.XXXX" when `turns_with_reported_cost > 0` else "—". Paste both invocations and outputs.
9. `nav-core` tests: schema applies on fresh temp DB; pragmas set; round-trip create+append+load; AssistantMessageDelta NOT persisted, Done IS; complete_turn(reported) rolls up cost_micros_reported + turns_with_reported_cost + turns_total; complete_turn(unreported) rolls up only turns_total; --resume integration test (slice A stub) matches fresh run plus one extra prompt. Paste each test and `cargo test -p nav-core` output.

Constraints — failing any means the goal is NOT met:
- Never compute cost from tokens × pricing. Every turn writes cost_source='unreported', cost_micros=NULL. Tokens recorded regardless.
- `provider` hardcoded "openai-responses". No ModelProvider trait (later slice).
- No TUI, no `nav-tui` crate.
- `.gitignore` covers `*.db`, `*.db-wal`, `*.db-shm`.
- Don't weaken tests or add `#[allow]` to silence clippy.

After 50 turns if end state is not met, stop and report the blocking item with exact command output. Don't loop on failing tests — diagnose and fix root cause or stop.
```

---

## Slice B — `nav-tui` crate + chat widget

**Run in a worktree** (`../nav-slice-b`) if running in parallel with A.5.

```
/goal Build the nav-tui crate skeleton with a chat widget that renders AgentEvent as HistoryCells.

End state — all of these must be demonstrably true in the transcript:

1. `cargo build --workspace` exits 0.
2. `cargo test --workspace` exits 0.
3. `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
4. `cargo fmt --check` exits 0.
5. New crate `crates/nav-tui` exists in the workspace with deps on
   ratatui, crossterm, tokio, and nav-core. Paste its Cargo.toml.
6. `nav-tui` defines a public `trait HistoryCell` with at least
   `fn display_lines(&self, width: u16) -> Vec<ratatui::text::Line<'static>>`
   and `fn desired_height(&self, width: u16) -> u16`. Paste the trait.
7. Concrete cell types implement HistoryCell for at least:
   UserMessageCell, AssistantMessageCell, ToolCallCell, ToolOutputCell,
   ErrorCell. Each one consumes data from an AgentEvent. Paste each
   `impl HistoryCell` block.
8. `nav-tui` defines a `ChatWidget` that owns `Vec<Box<dyn HistoryCell>>`
   and has `fn ingest(&mut self, event: AgentEvent)` and an impl of
   ratatui's `Widget` (or `WidgetRef`) trait. Paste the struct + impls.
9. A snapshot test using `insta` and `ratatui::backend::TestBackend`
   renders a fixed sequence of AgentEvents (user → tool call → tool
   output → assistant message → turn complete) and snapshots the final
   terminal buffer. Paste the test, paste `cargo test -p nav-tui`
   output, confirm the .snap file is committed.

Constraints — failing any of these means the goal is NOT met:
- Do not wire the TUI into the `nav` binary yet. `nav-cli/src/main.rs`
  must remain unchanged from slice A.
- Do not implement live streaming partition (stable/tail). Assistant
  messages render as a single cell after AssistantMessageDone.
- Do not add a composer / bottom pane / input handling yet.
- Do not add a tokio::select loop yet — this slice is pure rendering.
- Do not modify `nav-core` except to make `AgentEvent` public if it
  isn't already.
- Do not touch session storage — A.5 is a parallel slice; if it has
  not landed yet, do not introduce a placeholder.
- Do not weaken existing tests.
- Do not introduce `#[allow]` attributes to silence clippy.

If after 40 turns the end state is not satisfied, stop and report which
item is blocking with the exact command output. Do not loop on a
failing test — diagnose root cause and fix it, or stop.
```

---

## Slice C — streaming controller (stable / tail)

**Run in a worktree** (`../nav-slice-c`) if running in parallel with D.

```
/goal Add a streaming controller to nav-tui that partitions live assistant output into stable and tail regions.

End state — all of these must be demonstrably true in the transcript:

1. `cargo build --workspace` exits 0.
2. `cargo test --workspace` exits 0.
3. `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
4. `cargo fmt --check` exits 0.
5. `nav-tui` has a `streaming` module that exposes a `StreamController`
   (name negotiable) with at least:
     - `fn push_delta(&mut self, text: &str)`
     - `fn finalize(&mut self)`
     - `fn stable_lines(&self, width: u16) -> Vec<Line<'static>>`
     - `fn tail_lines(&self, width: u16) -> Vec<Line<'static>>`
   Paste the struct + impl.
6. The partition rule is documented in a doc-comment: lines that end
   with a hard newline AND aren't inside an unterminated markdown
   block (fenced code, table) move to stable; the rest stay in tail
   and re-render each delta. Paste the doc-comment.
7. Tables specifically are held in tail until finalize() — verified
   by a test that pushes a partial table and asserts `stable_lines()`
   does not contain the table.
8. `AssistantMessageCell` from slice B is updated to use
   `StreamController` for live deltas. When `AssistantMessageDone`
   fires, the controller is finalized.
9. Three insta snapshot tests cover: mid-stream prose, mid-stream
   table (held back), and post-finalize. Paste each test and
   `cargo test -p nav-tui` output.

Constraints — failing any of these means the goal is NOT met:
- Do not wire the TUI into the `nav` binary yet.
- Do not add the composer / bottom pane (that is slice D).
- Do not change AgentEvent's public shape.
- Do not weaken existing tests or modify slice B's snapshot files
  beyond what's required by the AssistantMessageCell change.
- Do not introduce `#[allow]` attributes to silence clippy.

If after 40 turns the end state is not satisfied, stop and report which
item is blocking with the exact command output. Do not loop on a
failing test — diagnose root cause and fix it, or stop.
```

---

## Slice D — bottom pane composer + slash commands

**Run in a worktree** (`../nav-slice-d`) if running in parallel with C.

```
/goal Build the bottom pane composer with slash commands for nav-tui.

End state — all of these must be demonstrably true in the transcript:

1. `cargo build --workspace` exits 0.
2. `cargo test --workspace` exits 0.
3. `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
4. `cargo fmt --check` exits 0.
5. `nav-tui` has a `bottom_pane` module with a `Composer` widget that
   supports:
     - Multi-line text editing with cursor movement (arrows, Home, End)
     - Enter submits, Shift+Enter inserts newline
     - Backspace, Delete, Ctrl+U (clear line), Ctrl+W (delete word)
     - Bracketed paste insertion as a single edit
     - Up/Down recall through submitted-prompt history
   Paste the public API (struct + key methods).
6. `bottom_pane` exposes a `BottomPaneView` trait or enum allowing
   overlays to push on top of the composer, with input routed
   view-first (overlay sees the key first, composer only if the
   overlay returns Unhandled). Paste the type.
7. A `SlashCommandPopup` overlay is implemented. It activates when the
   composer line starts with `/`, filters a fixed list ({`/help`,
   `/clear`, `/quit`, `/resume`, `/sessions`}) by prefix, and Tab/Enter
   completes the command. Paste the overlay impl.
8. Input is driven by an event type `ComposerEvent` (or AppEvent
   subset) that Composer's `handle_key` returns: `Submit(String)`,
   `Nothing`, `Cancelled`, etc. Paste the type.
9. Tests using `ratatui::backend::TestBackend` + simulated key events:
     - typing "hello" and pressing Enter returns Submit("hello")
     - typing "/" shows the popup; "/he" filters to "/help"
     - Shift+Enter does not submit
     - Up arrow recalls a previous prompt
   Paste each test and `cargo test -p nav-tui` output.

Constraints — failing any of these means the goal is NOT met:
- Do not wire the TUI into the `nav` binary yet (that is slice E).
- Do not implement actions for the slash commands — the overlay only
  needs to surface a chosen command. Wiring is slice E.
- Do not touch the streaming module from slice C.
- Do not weaken existing tests.
- Do not introduce `#[allow]` attributes to silence clippy.

If after 40 turns the end state is not satisfied, stop and report which
item is blocking with the exact command output. Do not loop on a
failing test — diagnose root cause and fix it, or stop.
```

---

## Slice E — wire `nav` binary, full app loop

Run only after A, A.5, B, C, D are merged.

```
/goal Wire nav-tui into the nav binary so `nav` launches an interactive TUI integrated with session storage.

End state — all of these must be demonstrably true in the transcript:

1. `cargo build --workspace` exits 0.
2. `cargo test --workspace` exits 0.
3. `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
4. `cargo fmt --check` exits 0.
5. `nav-cli/src/main.rs` selects mode:
     - tty AND no `--json-events`: launch the TUI.
     - `--json-events` flag: stream NDJSON AgentEvent to stdout.
     - non-tty AND no flag: default to NDJSON.
     - `--resume <id>` honored in both TUI and NDJSON modes.
   Paste the updated main.rs.
6. `nav_tui::run` sets up:
     - tokio runtime (or async usage of an existing runtime)
     - crossterm raw mode + bracketed paste + alt-screen
     - a panic hook that restores the terminal
     - a single `tokio::select!` loop over: crossterm events,
       AgentEvent stream from nav-core, internal AppEvent channel.
   Paste `nav_tui::run`'s body.
7. Hooked TUI behaviors (each covered by a vt100-based integration
   test or deterministic unit test — paste tests + output):
     - Enter in the composer submits a prompt; AgentEvents from
       nav-core appear in the transcript live.
     - `/quit` and Ctrl+C twice exit cleanly with the terminal
       restored; session status set to 'completed'.
     - `/clear` empties the transcript without exiting; current
       session status set to 'abandoned', a new session is created.
     - Streaming assistant deltas use the slice C controller.
8. Session integration:
     - Launching the TUI without `--resume` calls
       SessionStore::create_session(cwd, "openai-responses",
       <model from config>, <default profile>). The session id
       is visible in the TUI footer or status indicator.
     - Every durable AgentEvent is persisted via append_event.
     - TurnComplete triggers complete_turn with the TurnUsage
       payload and cost = None (cost remains unreported in nav).
     - `--resume <id>` loads events, renders them as history cells,
       then begins live event flow appended to the same session.
   Paste the integration test and its output.
9. Terminal-restore-on-panic is validated: a test spawns a child
   process that panics inside the TUI loop and the parent asserts no
   raw-mode escape sequences leak to the captured stdout/stderr.
   Paste the test and its output.
10. NDJSON regression check: run the same prompt that slice A's test
    used, in `--json-events` mode with stdout piped to a file, paste
    the captured NDJSON, and confirm the event sequence matches slice
    A's expected sequence.

Constraints — failing any of these means the goal is NOT met:
- Cost is never written as 'reported' here. Token counts ARE recorded
  per turn via the existing slice A.5 write path.
- Do not regress slice A's NDJSON behavior.
- Do not regress any slice B / C / D snapshot test.
- Do not introduce `#[allow]` attributes to silence clippy.

If after 50 turns the end state is not satisfied, stop and report which
item is blocking with the exact command output.
```

---

## After E

The schema and `AgentEvent` shape are forward-compatible with everything
below. None of these need a migration to land:

- **Slice F — ModelProvider trait extraction.** Refactor the OpenAI
  Responses code into an `OpenAiResponsesProvider` behind a
  `trait ModelProvider`. Add a `profile` resolver that reads
  `~/.nav/config.toml` (or `$XDG_CONFIG_HOME/nav/config.toml`). The
  schema's `provider` column starts being populated from
  `provider.id()` instead of the hardcoded string. Still only one
  provider exists at this point.

- **Slice G — first second-provider (OpenRouter).** OpenRouter is the
  natural first because it returns `usage.cost` per request, which
  exercises the `cost_source = 'reported'` write path. After this
  slice, sessions using OpenRouter show real dollar amounts in
  `--list-sessions` and the TUI footer.

- **Slice H+ — additional providers** (Deepseek, Z.ai, Anthropic
  direct, Ollama, etc.). Each is a new `ModelProvider` impl plus a
  config example. None require schema changes.

Other deferred TUI features (each its own slice when the backing
capability exists in nav-core): approval overlays, MCP cells, plan
rendering, multi-thread UI, model picker UI, file-search popup against
the real workspace, pager / transcript / backtrack overlays, voice /
audio, OSC 8 hyperlinks, theme picker, in-TUI session picker (lists
sessions from `SessionStore::list_sessions` and resumes the chosen one).
