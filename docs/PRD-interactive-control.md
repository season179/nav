# PRD: Interactive Control

## Problem Statement

When nav is busy running an assistant turn or tool call, the human loses
control of the session. The TUI currently refuses new prompts with an
"agent is busy" error, and the existing interrupt gesture is only wired
toward quitting rather than stopping or steering the active turn. That makes
nav feel brittle for ordinary coding work: the user cannot stop a bad command
quickly, add context while the model is working, queue a follow-up before
forgetting it, or see which pending requests will run next.

This blocks nav's daily-driver goal. A dependable coding agent needs a clear
control plane for "stop this", "add this context", and "do this next" without
racing the session log, provider transport, tool execution, approvals, or
visible transcript.

## Solution

Add real interactive control for active TUI sessions. While a turn is running,
nav should accept user input into an explicit pending queue instead of rejecting
it. The user should be able to choose whether a message is steering context for
the current run or a follow-up for after the current run completes. The TUI
should display the queue, let the user edit or remove queued messages, and
provide a direct abort action that stops the active model/tool work and leaves
the session in a comprehensible state.

From the user's perspective, nav should feel calm and interruptible:

- I can stop the current turn when it is wrong or taking too long.
- I can type the next instruction while nav is still working.
- I can see what is queued and adjust it before it runs.
- I can distinguish "steer this active task" from "do this after you finish".
- I can recover after an abort without corrupting the transcript or hidden
  session state.

## User Stories

1. As a developer using nav in a terminal, I want to abort the current assistant turn, so that I can stop a wrong or expensive path before it continues.
2. As a developer using nav in a terminal, I want to abort a currently running tool call, so that a stuck command does not trap the whole session.
3. As a developer reviewing an agent's direction, I want to send steering context while the agent is busy, so that the next model/tool boundary includes my correction.
4. As a developer thinking ahead, I want to queue a follow-up prompt while the agent is busy, so that I do not have to wait silently before typing the next task.
5. As a developer, I want queued follow-ups to run after the active turn settles, so that nav never starts two concurrent turns against the same session.
6. As a developer, I want steering messages and follow-up messages to be visually distinct, so that I understand what will be injected soon versus later.
7. As a developer, I want to see all pending input in the TUI, so that no hidden queued instruction surprises me.
8. As a developer, I want to edit the most recent queued message, so that I can fix wording without cancelling the whole queue.
9. As a developer, I want to remove a queued message, so that stale follow-ups do not run after the task changes.
10. As a developer, I want to clear all pending queued messages, so that I can reset my intent during a messy session.
11. As a developer, I want abort to preserve any useful typed draft or queued follow-up, so that stopping the agent does not throw away my work.
12. As a developer, I want abort to clear unsafe pending steering when appropriate, so that obsolete mid-turn instructions are not applied after a cancelled turn.
13. As a developer, I want the transcript to show that a turn was aborted, so that later session review explains why a response or tool result is incomplete.
14. As a developer, I want partial assistant output from an aborted turn to be handled consistently, so that resume and replay do not treat it as a normal completed answer.
15. As a developer, I want a running shell command to receive a real cancellation signal where possible, so that local processes stop rather than merely hiding their output.
16. As a developer, I want cancellation failures to be visible, so that I know when nav could not stop an external process cleanly.
17. As a developer, I want approval prompts to remain answerable while a turn is active, so that the new queue UI does not block safety decisions.
18. As a developer, I want abort to work from the same busy state that shows approval prompts, so that I can stop the session instead of approving or denying piecemeal.
19. As a keyboard-first user, I want a discoverable interrupt keybinding, so that stopping the agent is fast and does not require mouse-like interaction.
20. As a keyboard-first user, I want the existing quit behavior to remain safe, so that I do not accidentally exit nav when I meant to stop a turn.
21. As a user of slash skills, I want skill activations queued during a busy turn to remain attached to the intended queued prompt, so that skill context is not lost or applied to the wrong turn.
22. As a user attaching images or files, I want queued messages to preserve their attachments, so that follow-ups keep the context I selected.
23. As a user resuming a session later, I want completed, aborted, and queued states to replay clearly, so that the session history remains understandable.
24. As a future desktop frontend, I want queue and abort behavior represented in structured events, so that non-TUI surfaces can mirror the same controls.
25. As a future chat frontend, I want follow-up queue behavior to be protocol-level rather than terminal-only, so that remote sessions can accept messages while work is running.
26. As a maintainer, I want the queue and abort logic isolated behind small interfaces, so that the behavior can be tested without driving the full terminal UI.
27. As a maintainer, I want deterministic tests for queue draining and abort outcomes, so that future TUI changes do not reintroduce "agent is busy" dead ends.
28. As a maintainer, I want clear event names for queued, dequeued, aborted, and completed states, so that session persistence and downstream frontends stay stable.

## Implementation Decisions

- Build a session-level control plane that owns the active turn handle,
  cancellation token, and pending input queues. Keep it small and explicit so
  the TUI, CLI, and future frontends can call the same operations.
- Split pending input into at least two modes: steering messages that are
  injected at the next safe model/tool boundary, and follow-up messages that
  run after the active turn completes or aborts.
- Do not allow two live assistant turns to mutate the same session
  concurrently. Queued follow-ups are serialized behind the active turn.
- Treat abort as a first-class turn outcome, not as a generic error. It should
  emit a visible event, persist a transcript marker, stop further tool dispatch
  for that turn, and leave the session ready for a new prompt.
- Propagate cancellation through the model transport, tool runner, and shell
  execution path where supported. If a subsystem cannot cancel immediately, it
  must report that limitation visibly.
- Preserve approval handling as an interrupt surface. Approval prompts should
  remain higher priority than ordinary queued follow-ups, but the user should
  still be able to abort the active turn from an approval state.
- Add queue events to the agent event stream so non-TUI consumers can show the
  same pending state. The TUI should render these events rather than keeping
  queue truth purely in local widget state.
- Keep the pending queue editable in the composer layer. Editing and cancelling
  queued messages should update the canonical queue state, not only the
  rendered preview.
- Keep skill activation semantics intact. A queued standalone skill should stay
  bound to the next queued prompt that uses it, and an inline skill request
  should queue as a single logical user message.
- Prefer a deep module for pending input state: enqueue, edit, remove, clear,
  drain-for-steering, drain-for-follow-up, and preview should be testable
  without terminal rendering.
- Prefer a deep module for turn control: start, abort, mark-complete,
  mark-failed, and query active state should be testable without real provider
  calls.
- Make keybindings discoverable in existing status/help surfaces, but do not
  make the PRD depend on a single exact key. The implementation should preserve
  a safe quit path while adding a fast interrupt path.

## Testing Decisions

- Good tests should assert external behavior: queued input is accepted while a
  turn is active, pending state is visible, abort produces the right session
  events, and queued messages drain in the right order. Tests should avoid
  asserting private struct layout unless the module is intentionally a deep
  unit-tested module.
- Unit-test the pending input queue for ordering, mode separation, editing,
  removal, clearing, attachment preservation, and preview data.
- Unit-test the turn control layer for active-state transitions, abort
  idempotency, completion after abort, and queued follow-up dispatch after a
  turn settles.
- Add agent-loop tests with a stub transport to prove steering messages are
  injected at safe boundaries and follow-ups run only after normal completion
  or abort handling.
- Add tool-runner tests with a fake long-running tool or shell runner to prove
  abort requests stop additional tool dispatch and surface an aborted result.
- Add TUI input tests for submitting while busy, editing queued input, clearing
  queued input, and preserving approval prompt behavior.
- Add snapshot tests for the pending queue preview so steering, rejected
  steering, and follow-up states are visually distinct.
- Add session persistence/replay tests so aborted turns and queued/drained
  messages do not corrupt resume history.
- Use the existing TUI snapshot and input tests as prior art for rendering and
  key handling.
- Use existing agent-loop and tool permission tests as prior art for event
  sequencing, tool outcomes, and approval interactions.

## Out of Scope

- Streaming assistant output live in the TUI, except where queue/abort UI needs
  to reflect active state.
- Full interactive session management such as a resume picker, named sessions,
  and transcript export.
- Long-session compaction and replay redesign beyond preserving aborted turn
  semantics.
- A provider adapter boundary or multi-provider support.
- Background task boards, multi-agent orchestration, or detached worker queues.
- Desktop and chat UI implementation, beyond ensuring the event stream can
  support them later.
- Broad redesign of the TUI visual system.

## Further Notes

- The current TODO marks this as the top daily-driver blocker because nav
  already has safety, diff, context, and reliability foundations, but still
  rejects input while busy.
- Codex is useful prior art for separating queued input, pending steering, and
  interrupt UI into focused state modules.
- Pi is useful prior art for exposing steering, follow-up, queue updates, and
  abort as explicit agent operations.
- The first valuable vertical slice is: accept a follow-up while busy, show it
  visibly, allow editing/removal, and run it after the active turn completes.
  Abort and steering can layer on the same control-plane foundation.
