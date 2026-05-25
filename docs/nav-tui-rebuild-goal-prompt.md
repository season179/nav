# nav-tui Rebuild Goal Prompt

Use this prompt in Codex when you want a long-running goal to rebuild nav's
deleted TUI from the sibling Codex implementation.

```text
/goal Rebuild nav's deleted `crates/nav-tui` from scratch by porting the terminal UI architecture from the sibling checkout at `/Users/season/Personal/codex/codex-rs/tui`, without stopping until `nav` has a working interactive TUI again and the verification loop passes.

Context:
- Work only in `/Users/season/Personal/nav`.
- Treat `/Users/season/Personal/codex` as a read-only reference. Do not edit it.
- `crates/nav-tui` has been deleted, but `Cargo.toml` still lists it as a workspace member and `crates/nav-cli` still depends on `nav-tui`.
- The target is not a line-for-line vendoring of Codex. Port Codex's proven TUI mechanisms into nav: inline terminal viewport, native scrollback insertion, custom terminal/buffer flushing, live streaming transcript, composer/status area, and event-driven app loop.
- Keep nav-core as the source of truth for agent loop, events, auth, settings, sessions, approvals, tools, and model transport. Do not replace nav-core with Codex core/app-server/auth/config systems.
- Do not use old nav TUI docs as source of truth. Some docs have been deleted or are intentionally stale. Derive the target behavior from live nav code plus the sibling Codex TUI implementation.

Read first:
- `Cargo.toml`
- `crates/nav-cli/Cargo.toml`
- `crates/nav-cli/src/main.rs`
- `crates/nav-core/src/agent_loop/events.rs`
- `crates/nav-core/src/cli/mod.rs`
- `crates/nav-core/src/guardrails/approval/mod.rs`
- `/Users/season/Personal/codex/codex-rs/tui/src/lib.rs`
- `/Users/season/Personal/codex/codex-rs/tui/src/tui.rs`
- `/Users/season/Personal/codex/codex-rs/tui/src/custom_terminal.rs`
- `/Users/season/Personal/codex/codex-rs/tui/src/insert_history.rs`
- `/Users/season/Personal/codex/codex-rs/tui/src/app.rs`
- `/Users/season/Personal/codex/codex-rs/tui/src/history_cell/`
- `/Users/season/Personal/codex/codex-rs/tui/src/bottom_pane/`
- `/Users/season/Personal/codex/codex-rs/tui/src/streaming/` or current Codex streaming modules

Implementation contract:
1. Recreate `crates/nav-tui` as a real Rust crate with a public `nav_tui::run(...)` API compatible with the existing call in `crates/nav-cli/src/main.rs`, unless a smaller signature change is clearly justified and all call sites are updated.
2. Port the Codex terminal substrate first: custom terminal, inline viewport setup/restore, history insertion into native scrollback, wrap/reflow helpers, and the redraw invalidation behavior needed when viewport geometry changes.
3. Build a thin nav adapter over `nav_core::AgentEvent` instead of importing Codex protocol types. Map nav events into TUI history cells, active streaming cells, tool/approval/status cells, and error cells.
4. Restore the essential interactive flow:
   - launch `nav` in a terminal and show startup/session context
   - accept typed prompts and initial CLI prompt
   - stream assistant text live
   - finalize assistant/tool/system events into native scrollback
   - keep composer and status visible in the inline viewport
   - support Ctrl+C abort/exit behavior, Ctrl+L redraw/clear behavior, Enter submit, paste, basic editing, and terminal resize
   - keep headless `--json-events` and `--json-rpc` behavior untouched
5. Prefer Codex's current rendering mechanisms over old deleted nav-tui assumptions. Do not resurrect old nav-tui code from git except as behavioral reference if needed.
6. Keep the dependency surface reasonable. If copying a Codex file pulls in Codex-only systems, either adapt it to nav types or cut that dependency away.
7. Do not restore deleted docs or make old docs part of the implementation plan. If a stale doc blocks a build or test, make the smallest necessary edit and explain why.

Verification loop:
- After each checkpoint, run the narrowest useful compile/test command.
- Final verification must include:
  - `cargo fmt --all -- --check`
  - `cargo test -p nav-core`
  - `cargo test -p nav-tui`
  - `cargo test -p nav-cli`
  - `cargo test -p nav-cli --test tmux_viewport -- --nocapture`
  - `cargo clippy --workspace --all-targets -- -D warnings`
- Add or repair tmux-backed regression coverage for the rebuilt TUI. If tmux is unavailable, skip cleanly in the test and explicitly report that only the skip path was exercised.
- Also run `cargo run -p nav-cli -- --help` and at least one interactive/tmux smoke test proving the real `nav` binary paints a nonblank TUI, accepts a prompt, streams/finalizes output, and preserves the composer/status area.

Stop only when:
- `crates/nav-tui` exists again and builds as part of the workspace.
- `crates/nav-cli` launches the rebuilt TUI in terminal mode.
- The essential interactive flow above works through the real binary.
- The final verification loop passes, or any remaining failure is clearly unrelated/pre-existing with exact evidence.
- A concise final report lists what was ported from Codex, what was intentionally simplified, the commands run, and any residual risk.

Pause and ask only if:
- `/Users/season/Personal/codex/codex-rs/tui` is missing,
- nav-core lacks an event needed for basic TUI behavior and adding it would change the agent protocol,
- or the only path forward would require deleting/replacing nav-core instead of adapting the TUI to it.
```
