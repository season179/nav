# AgentMessageCell Goal Prompt

Use this prompt with Codex `/goal` when continuing the assistant-message
streaming and line-spacing fix.

```text
/goal Fix nav's AgentMessageCell / streaming assistant line-spacing bug end-to-end without stopping until there is a reproducible measurement harness and a verified fix.

Work in /Users/season/Personal/nav and follow AGENTS.md. Do not spend time on WebSocket/SSE transport unless a test proves the bug is transport-specific. Treat Codex as the behavioral reference, but do not declare success from source similarity.

First read:
- docs/agent-message-lifecycle.md
- docs/codex-tui-component-reference.md
- crates/nav-tui/src/cells/messages.rs
- crates/nav-tui/src/streaming/controller.rs
- crates/nav-tui/src/chat.rs
- crates/nav-cli/tests/tmux_viewport.rs
- ../codex/codex-rs/tui/src/history_cell/messages.rs
- ../codex/codex-rs/tui/src/streaming/controller.rs
- ../codex/codex-rs/tui/src/chatwidget/streaming.rs

Stopping condition:
1. Add or adjust a deterministic failing test that reproduces the bad spacing through the real streaming lifecycle: delayed chunks, Markdown blank lines / lists, stable AgentMessageCell chunks leaving the live viewport, final consolidation, and no skipped or duplicated rows.
2. Include a tmux-backed regression in crates/nav-cli/tests/tmux_viewport.rs when tmux is available. If tmux is unavailable, skip cleanly and report that.
3. Prove the test fails on the broken behavior, either on the pre-fix code or by temporarily reverting the relevant fix, then restore the fix.
4. Implement the narrowest renderer/controller/widget change needed. Do not change WebSocket/SSE transport.
5. Verify with:
   - cargo test -p nav-tui streaming
   - cargo test -p nav-tui --test streaming_characterization
   - cargo test -p nav-cli --test tmux_viewport -- --nocapture
   - git diff --check
   - cargo test --workspace if the focused tests are clean and time permits
6. Final report must include failing-before/passing-after evidence, exact commands run, and any skipped checks.

Work in checkpoints. Keep progress updates short and evidence-based. Do not claim "fixed" until the stopping condition is satisfied.
```
