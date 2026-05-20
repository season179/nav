# Long-Session Compaction PRD

## Problem Statement

nav is meant to become a dependable daily coding agent, but long sessions are still fragile. Today, when a session grows too large, nav can recover from a context-window overflow by dropping an old tool-call pair and retrying once. That keeps a turn from failing in some cases, but it is invisible to the user, lossy, and not a real continuity strategy.

As a user, I need nav to preserve the useful state of a long task while reducing the model-visible transcript. I should be able to ask for compaction manually, trust nav to compact automatically before long-session failure, resume after compaction, and understand what was retained.

## Solution

Add first-class long-session compaction modeled after Codex's compaction behavior.

Compaction should be a normal session lifecycle event, not an emergency truncation. nav should create a concise handoff summary of the session, replace older model-visible history with that summary plus selected recent user context, persist the checkpoint locally, and replay future turns from the compacted checkpoint. Users should be able to run `/compact` manually, and nav should also compact automatically when token usage crosses a configured threshold.

The default compaction prompt should follow Codex's shape: ask the model to produce a context checkpoint handoff for another LLM, including current progress, key decisions, constraints, user preferences, remaining work, and critical references needed to continue.

## User Stories

1. As a daily nav user, I want to run `/compact`, so that I can intentionally shorten a long session before continuing.
2. As a daily nav user, I want nav to compact automatically near the context limit, so that a long task does not fail unexpectedly.
3. As a daily nav user, I want compaction to preserve key decisions, so that the agent does not reopen settled choices.
4. As a daily nav user, I want compaction to preserve remaining tasks, so that the agent can continue from the right next step.
5. As a daily nav user, I want compaction to preserve important user constraints, so that the agent keeps following my preferences after compaction.
6. As a daily nav user, I want compaction to preserve critical file, command, and test references in summary form, so that the next turn stays grounded.
7. As a daily nav user, I want recent user messages to remain visible to the model when appropriate, so that the summary does not erase immediate instructions.
8. As a daily nav user, I want compacted sessions to resume correctly, so that restarting nav does not lose the checkpoint.
9. As a daily nav user, I want old visible scrollback to remain available, so that compaction does not feel like deleting my transcript.
10. As a daily nav user, I want model-visible replay to use the compacted checkpoint, so that future turns are smaller and more reliable.
11. As a daily nav user, I want a visible TUI event when compaction starts, so that I understand why nav is busy.
12. As a daily nav user, I want a visible TUI event when compaction completes, so that I can see that the session was checkpointed.
13. As a daily nav user, I want compaction failure to be explicit, so that I know whether the next turn is still using the old history.
14. As a daily nav user, I want queued follow-up prompts to wait during manual compaction, so that I can keep typing without losing input.
15. As a daily nav user, I want nav to avoid steering a compaction turn, so that the checkpoint remains coherent.
16. As a daily nav user, I want `/compact` to be listed in slash commands, so that the capability is discoverable.
17. As a daily nav user, I want automatic compaction to happen before submitting an over-large prompt when possible, so that the incoming prompt is not sacrificed.
18. As a daily nav user, I want automatic compaction to happen mid-flow when tool follow-up is required, so that long tool loops can continue safely.
19. As a daily nav user, I want token thresholds to be configurable, so that I can tune compaction for different models.
20. As a daily nav user, I want sane defaults per model, so that compaction works without configuration.
21. As a daily nav user, I want compaction to work with local session storage, so that nav remains local-first and does not depend on server-side conversation state.
22. As a daily nav user, I want compaction to preserve encrypted reasoning or provider continuation artifacts only when required and safe, so that future tool turns remain valid.
23. As a daily nav user, I want compaction to avoid storing huge generated summaries repeatedly, so that the local database stays reasonable.
24. As a daily nav user, I want compacted checkpoints to be inspectable in exported transcripts later, so that I can audit how the session evolved.
25. As a TUI user, I want compacted checkpoints to render as compact history rows, so that the transcript is readable.
26. As an NDJSON consumer, I want stable compaction events, so that non-TUI frontends can show compaction consistently.
27. As a future frontend implementer, I want compaction events to be protocol-level events, so that each UI does not infer compaction from text.
28. As a maintainer, I want compaction logic isolated behind a small interface, so that summary generation, history replacement, and replay can be tested separately.
29. As a maintainer, I want automatic compaction decisions tested against token accounting, so that threshold behavior does not drift.
30. As a maintainer, I want replay tests for compacted sessions, so that resume cannot silently expand back to the old full transcript.
31. As a maintainer, I want failure-path tests, so that failed compaction does not corrupt persisted session history.
32. As a maintainer, I want the old overflow recovery to become a fallback rather than the primary strategy, so that context pressure is handled intentionally.

## Implementation Decisions

- Build a dedicated compaction module in the core agent library. It should expose a small interface for manual compaction, automatic compaction checks, summary generation, replacement-history construction, and replay checkpoint selection.
- Represent compaction as first-class agent events. At minimum, nav needs events for compaction start, compaction completion, and compaction failure. Completion should include the persisted summary text and enough metadata to distinguish manual and automatic compaction.
- Persist compaction checkpoints in local SQLite. A checkpoint should be durable across resume and tied to the session event sequence.
- Keep visible transcript history separate from model-visible replay history. The user should still see old events, but future model input should begin from the latest compacted checkpoint plus subsequent durable events.
- Use Codex's compaction prompt shape as the default summary prompt. The summary should be a handoff for another LLM, focused on progress, decisions, constraints, remaining work, and critical references.
- Build replacement history from the compaction summary plus selected recent real user messages. Summary messages should not be repeatedly summarized as if they were real user instructions.
- Preserve local-first behavior. nav should continue using local session state and `store: false`; compaction must not require provider-side stored conversation state.
- Add `/compact` as a built-in slash command. It should run as a non-steerable compaction turn, and follow-up prompts submitted while it is running should queue for the next normal turn.
- Add automatic compaction before a new turn when the recorded or estimated model-visible token usage crosses the configured threshold.
- Add automatic mid-turn compaction when token usage crosses the threshold and the model or queued input still needs a follow-up turn.
- Add a configurable automatic compaction token limit. The limit may come from model metadata when available and should be overridable through nav settings or CLI arguments.
- Keep the existing one-shot context overflow trimming path as a last-resort fallback. It should not be the normal long-session behavior after compaction lands.
- Make compaction replay behavior explicit: after a checkpoint, replay should use the checkpoint summary and only events after the checkpoint, not the full pre-compaction transcript.
- Keep compaction summaries small enough to be useful. If the compaction request itself overflows, nav should trim oldest history while preserving recent messages and retry before failing.
- Emit a user-visible warning after successful compaction that long threads can still become less accurate and that fresh sessions remain better for unrelated tasks.
- Do not make provider adapters part of this PRD. The first implementation should target the existing OpenAI Responses flow, while avoiding unnecessary coupling that would block a future adapter boundary.

## Testing Decisions

- Good tests should verify external behavior: emitted agent events, persisted session rows, replay input shape, slash-command behavior, and automatic threshold decisions. They should not assert private helper structure unless that helper is intentionally a deep module.
- Core compaction tests should cover manual compaction with prior history, manual compaction with little or no prior history, automatic pre-turn compaction, automatic mid-turn compaction, compaction failure, and compaction request overflow recovery.
- Replay tests should prove that a resumed compacted session sends the checkpoint summary plus post-checkpoint events, not the full old transcript.
- Session-store tests should prove compaction events round-trip through local SQLite and remain ordered with normal user, assistant, tool, and turn-complete events.
- TUI tests should cover `/compact` dispatch, queued user messages during compaction, visible compaction rows, and non-steerable compact turn behavior.
- NDJSON tests should cover stable serialized compaction event shapes for non-TUI frontends.
- Existing resume, session, and agent-loop tests are the closest prior art. New tests should follow those patterns rather than introducing a separate harness.

## Out of Scope

- Provider-neutral adapter extraction.
- Remote or server-side compaction APIs.
- Transcript export UI beyond ensuring compacted checkpoints are represented in the stored event stream.
- Session fork, branch tree, rollback, or checkpoint restore workflows.
- Prompt customization UI beyond a local configuration or CLI override.
- Subagents, multi-agent summarization, or cross-session memory.
- Rewriting the entire session store or TUI architecture.

## Further Notes

- This PRD intentionally treats Codex's compaction behavior as the reference implementation.
- The minimum useful slice is manual `/compact` plus persisted replay from a checkpoint. Automatic threshold compaction should follow in the same feature track because it is part of the daily-driver blocker.
- The most important product property is trust: after compaction, nav should make it obvious what happened, preserve enough context to keep working, and never pretend a failed checkpoint succeeded.
