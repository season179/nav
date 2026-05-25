# T3 Code Frontend for nav — PRD

## Status

Proposed. Records the product and engineering shape agreed in discussion.
Do not implement as part of this document-writing pass.

## Problem Statement

`nav`’s Rust harness (`nav-core`) is the learning and product focus, but
`nav-tui` consumes disproportionate effort. The inline viewport, native
scrollback injection, ratatui buffer diffs, streaming cell lifecycle, and
tmux-backed regression requirements create recurring frontend work that does
not advance the agent loop, tools, context management, or guardrails.

The goal is to **stop wrestling with the terminal frontend** and **spend more
time building the nav agent backend**, while still having a usable daily-driver
interface.

## Product Decision

Adopt a **fork of [T3 Code](https://github.com/pingdotgg/t3code)** (MIT) as
nav’s primary UI. Add a custom **`nav` provider driver** in that fork that
runs the existing `nav` binary in headless mode and speaks the stable
**`--json-rpc`** protocol on stdio.

`nav-core` remains the brain. The fork owns presentation, session chrome,
streaming layout, approvals UI, and desktop/web packaging. **`nav-tui` is
deprecated** for new feature work and retired once the fork reaches daily-driver
quality.

Nav-specific terminal UX (inline scrollback, tmux viewport tests, native
terminal resize behavior) is **out of scope** for the new frontend.

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│  t3code fork (Bun server + React web + Electron desktop) │
│  ┌─────────────┐    ┌──────────────────────────────────┐ │
│  │ apps/web    │◄──►│ apps/server                      │ │
│  │ (React UI)  │ WS │  orchestration + NavDriver       │ │
│  └─────────────┘    └──────────────┬───────────────────┘ │
└────────────────────────────────────┼─────────────────────┘
                                     │ spawn per thread/session
                                     │ stdin/stdout NDJSON
                                     ▼
┌─────────────────────────────────────────────────────────┐
│  nav-cli  `nav --json-rpc`                              │
│  ┌─────────────┐                                        │
│  │ nav-core    │  agent loop, tools, replay, compaction │
│  └─────────────┘                                        │
└─────────────────────────────────────────────────────────┘
```

**Seams (unchanged from nav’s model):**

| Layer | Responsibility |
|-------|----------------|
| `nav-core` | Agent loop, `AgentEvent` emission, tools, guardrails, session persistence |
| `nav --json-rpc` | Versioned JSON-RPC notifications on stdout; approval responses on stdin |
| `NavDriver` (fork) | Process lifecycle, protocol bridge, map `AgentEvent` ↔ orchestration events |
| t3code web/desktop | Transcript, composer, approvals, connection UX |

**Wire protocol (v1):** use existing headless contract in `nav-core`:

- `nav.session.started` — session metadata (id, cwd, model, transport)
- `nav.event` — wraps each `AgentEvent`
- `nav.approval.respond` — stdin JSON-RPC for approval decisions

Protocol version: `HEADLESS_PROTOCOL_VERSION` (currently `1`). Bump only with
an explicit migration plan in the driver.

## Goals

1. **Daily-driver GUI** for nav via t3code web or desktop, without maintaining
   `nav-tui`.
2. **Backend-first development:** most commits land in `nav-core` / `nav-cli`;
   fork changes are limited to the provider adapter and thin UI gaps.
3. **v1 covers core agent loop UX:** submit prompt, stream assistant output,
   show tool calls and outputs, handle approval prompts, show errors and turn
   completion.
4. **Preserve nav behavior** for everything the driver does not surface in UI
   (compaction, replay, skills injection, etc. still run in `nav-core`).
5. **Local-first:** no cloud dependency; run server + `nav` on the machine
   under the user’s cwd.
6. **Honest deprecation path** for `nav-tui` (freeze → optional fallback →
   remove).

## Non-Goals

### v1 — explicitly deferred

These nav features **may continue to work in the harness** but need not appear
in the t3code UI until a later phase:

- Skills catalog slash popup and skill-invocation cells
- Session tree, fork/resume picker, labels, transcript search UI
- `/compact`, `/context`, `/handoff`, and other slash-command surfaces
- Reasoning collapse UI (reasoning may stream as plain text or be hidden)
- Subagent cards, git checkpoint rows, pending-input queue UI
- Turn diff / full-thread diff presentation beyond basic tool output
- Nav-specific tmux or inline-viewport behavior
- Upstreaming the nav driver to pingdotgg/t3code (fork-only)

### Permanent non-goals

- Replacing `nav-core` with t3code’s Codex/Claude/OpenCode providers for nav
  sessions (nav sessions use **nav**, not Codex app-server).
- Merging the two repos into one codebase.
- Re-implementing nav’s agent loop in TypeScript.

## User Stories

### v1 (must have)

1. As a nav user, I open the t3code fork (web or desktop), point it at a
   project directory, and start a thread backed by **nav** so I can work without
   the terminal TUI.
2. As a nav user, I type a prompt and see assistant text stream in real time.
3. As a nav user, I see tool calls and tool results in the transcript so I
   can follow what the agent did.
4. As a nav user, when nav requests approval, I can accept or decline in the UI
   and the turn continues or stops correctly.
5. As a nav user, I see clear errors when nav or the provider fails (auth,
   model, transport, tool errors).
6. As a nav maintainer, I can change `nav-core` and verify behavior through the
   fork without editing ratatui code.

### Later (should have)

7. As a nav user, I can resume a previous nav session from the GUI.
8. As a nav user, I can switch models and approval policy from settings exposed
   to the driver.
9. As a nav user, I can use nav slash commands or skills from the composer.
10. As a nav user, I can inspect token usage and turn boundaries in the UI.

## Functional Requirements

### NavDriver — process management

| ID | Requirement |
|----|-------------|
| D1 | Driver spawns `nav --json-rpc` with cwd set to the thread’s working directory. |
| D2 | Driver passes model, transport, and approval/sandbox flags via CLI args or env consistent with `nav-cli` defaults. |
| D3 | One nav subprocess per active provider session (or documented sharing model if t3code reuses sessions). |
| D4 | Driver terminates subprocess on session stop; handles crash/exit with `ProviderAdapterSessionClosedError`. |
| D5 | Driver reads stdout line-by-line as NDJSON; ignores malformed lines with logged warnings. |

### NavDriver — protocol bridge

| ID | Requirement |
|----|-------------|
| P1 | On `nav.session.started`, register session metadata with orchestration (session id, cwd, model). |
| P2 | On `nav.event`, map each `AgentEvent` variant to one or more orchestration domain events (see mapping appendix). |
| P3 | Unmapped events are logged at debug and do not crash the server. |
| P4 | User send-turn sends prompt text (and attachments if/when supported) by writing to nav stdin or CLI invocation per chosen spawn model. |
| P5 | Approval requests from nav emit orchestration “request” events; user decisions call `nav.approval.respond` on stdin. |
| P6 | Interrupt/abort maps to nav’s abort semantics (signal or control message as implemented in `nav-cli`). |

### Event mapping — v1 minimum

| `AgentEvent` (nav) | v1 UI behavior |
|--------------------|----------------|
| `UserMessage` | User message bubble |
| `AssistantMessageDelta` | Streaming assistant text |
| `AssistantMessageDone` | Finalize assistant message |
| `ToolCallStarted` | Tool call row (name + args summary) |
| `ToolCallOutput` | Tool result row (truncated display) |
| `ToolCallApprovalRequest` | Approval modal / inline prompt |
| `ToolCallApprovalDecision` | Audit row or silent (driver choice) |
| `ToolCallBlocked` | Error/denied row |
| `Error` | Error banner or thread error state |
| `TurnComplete` | Turn boundary + optional usage snippet |
| `TurnAborted` | Turn ended state |
| `ProviderRetry` | Transient status or toast |

All other `AgentEvent` variants: **accept and ignore for UI** in v1; still
persisted by nav’s session log.

### t3code fork — registration

| ID | Requirement |
|----|-------------|
| R1 | Add `NavDriver` to `BUILT_IN_DRIVERS` (or fork equivalent registry). |
| R2 | `ProviderDriverKind` slug: `nav` (or `navAgent`). |
| R3 | Default instance config: path to `nav` binary (default `nav` on `PATH`). |
| R4 | Settings UI (minimal): binary path, default model, transport, approval policy. |

### nav repo — harness stability

| ID | Requirement |
|----|-------------|
| N1 | `--json-rpc` protocol remains stable for v1; breaking changes require version bump and driver update. |
| N2 | Integration tests in **nav** that spawn `nav --json-rpc` with a mock or stub model remain green. |
| N3 | Document driver contract in nav README or `docs/` pointing to this PRD. |

### nav-tui — deprecation

| ID | Requirement |
|----|-------------|
| T1 | After v1 daily-driver sign-off, freeze `nav-tui` (no new features). |
| T2 | `nav` default when stdout is a TTY: either launch t3code fork (if bundled) or print message directing user to desktop/web (product choice at implementation). |
| T3 | Remove `nav-tui` crate only when fork coverage and CI no longer depend on it. |

## Success Metrics

| Metric | Target |
|--------|--------|
| Personal daily use | Use fork for real tasks for 2+ weeks without opening `nav-tui` |
| nav-tui churn | Zero net LOC growth in `nav-tui` after freeze |
| Backend focus | ≥70% of nav repo commits touch `nav-core` / tools / context / guardrails |
| v1 completeness | Core loop (prompt → stream → tools → approval → done) works on macOS desktop + web against local server |
| Regression | Nav driver has automated tests for protocol parsing and event mapping |

## Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| Event mapping drifts from `AgentEvent` | Versioned mapping table in driver; contract tests with fixture NDJSON from nav |
| Two-process debugging is awkward | Structured logs correlating `threadId` ↔ nav `session_id` |
| t3code upstream breaking fork | Pin fork; periodic merge cadence; small driver surface |
| Nav-only features invisible in UI | Document CLI fallbacks (`nav /compact`, etc.) until phase 2 |
| Auth (`~/.codex/auth.json`) confusion | Reuse nav’s existing auth; surface auth errors in t3code connection UI |
| User expects terminal workflow | Keep optional `nav-tui` until explicit cutover |

## Phased Delivery

### Phase 0 — Spike (1–2 weeks)

- Fork t3code locally.
- Minimal `NavDriver`: spawn `nav --json-rpc`, one prompt, print events to server log.
- Prove approval round-trip (mock approval request).
- **Exit:** recorded NDJSON fixture + mapping notes.

### Phase 1 — v1 daily driver (target: 4–6 weeks)

- Full v1 event mapping table (minimum rows above).
- Composer send, streaming assistant, tool rows, approval UI.
- Desktop or web launch documented for Season’s machine.
- Freeze `nav-tui`.
- **Exit:** personal daily use without `nav-tui`.

### Phase 2 — Parity gaps (as needed)

- Session resume, model/settings picker, token display.
- Skills and high-value slash commands in composer.
- Reasoning presentation, subagent rows, diffs.

### Phase 3 — Cleanup

- Default entrypoint story for `nav` binary.
- Remove or archive `nav-tui` crate.
- Drop tmux viewport test requirement for removed code paths.

## Open Questions

1. **Spawn model:** long-lived `nav --json-rpc` process per thread vs one-shot per turn?
2. **Default `nav` invocation:** keep `nav` opening TUI until phase 3, or immediately print “use T3 Code for nav”?
3. **Binary distribution:** require `nav` on `PATH` vs bundle nav inside Electron app?
4. **Attachments:** v1 text-only prompts or wire `UserMessage` attachments immediately?
5. **Fork naming/branding:** public fork vs private; relationship to upstream T3 Code stated in README?

## Appendix A — Reference repos

| Repo | Role |
|------|------|
| `../t3code` | UI + server fork base |
| `nav` | Harness + `--json-rpc` contract |
| `../codex` | Event shape reference (read-only) |
| t3code `CodexDriver` / `OpenCodeDriver` | Provider driver patterns |

## Appendix B — nav headless methods (current)

From `crates/nav-core/src/agent_loop/protocol.rs`:

- `nav.session.started`
- `nav.event`
- `nav.approval.respond` (stdin)

## Appendix C — Related nav docs

- [docs/CONTEXT.md](./CONTEXT.md) — frontend seams and architecture
- [docs/tui-component-reference.md](./tui-component-reference.md) — deprecated TUI (reference only)
- [README.md](../README.md) — `--json-rpc` usage examples
