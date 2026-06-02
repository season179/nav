import type {
  ChatMessage,
  SessionState,
  ToolMessage,
} from "../desktop/electron/renderer/src/types.ts";

const assert = require("node:assert/strict");
const { test } = require("node:test");

function loadSessionRuntime() {
  return import("../desktop/electron/renderer/src/lib/session-runtime.ts");
}

test("appended messages get unique ids per session", async () => {
  const { createSessionState, appendMessage } = await loadSessionRuntime();

  let state = createSessionState();
  state = appendMessage(state, "user", "hello");
  state = appendMessage(state, "assistant", "hi");

  assert.deepEqual(
    state.messages.map((message) => message.role),
    ["user", "assistant"],
  );
  assert.deepEqual(
    state.messages.map((message) => message.id),
    ["message-1", "message-2"],
  );
  assert.equal(state.messageSeq, 2);
});

test("tool lines update in place by tool call id", async () => {
  const { createSessionState, upsertToolLine } = await loadSessionRuntime();

  let state = createSessionState();
  state = upsertToolLine(state, "call-1", "running", "bash");
  state = upsertToolLine(state, "call-2", "running", "read");
  state = upsertToolLine(state, "call-1", "done", "bash", "ok");

  assert.equal(state.messages.length, 2, "same call id reuses its line");
  const [first, second] = state.messages as ToolMessage[];
  assert.equal(first.toolCallId, "call-1");
  assert.equal(first.state, "done");
  assert.equal(first.detail, "ok");
  assert.equal(first.id, "message-1", "id is preserved across updates");
  assert.equal(second.toolCallId, "call-2");
  assert.equal(second.state, "running");
});

test("run lifecycle drives running and clears stop", async () => {
  const { createSessionState, reduceSessionState } = await loadSessionRuntime();

  let state = { ...createSessionState(), stopPending: true };
  state = reduceSessionState(state, { type: "run.started" });
  assert.equal(state.running, true);

  state = reduceSessionState(state, { type: "run.completed" });
  assert.equal(state.running, false);
  assert.equal(state.stopPending, false);
});

test("run.failed records the error and ends the run", async () => {
  const { createSessionState, reduceSessionState } = await loadSessionRuntime();

  let state = reduceSessionState(createSessionState(), { type: "run.started" });
  state = reduceSessionState(state, {
    type: "run.failed",
    error: "model exploded",
  });

  assert.equal(state.running, false);
  const last = state.messages.at(-1) as ChatMessage | undefined;
  assert.equal(last?.role, "error");
  assert.equal(last?.text, "model exploded");
});

test("an event only touches the session it names", async () => {
  const { reduceSessions } = await loadSessionRuntime();

  let states: Record<string, SessionState> = {};
  states = reduceSessions(states, {
    type: "user.message",
    session_id: "a",
    text: "from a",
  });
  const sessionAfterA = states.a;

  states = reduceSessions(states, {
    type: "user.message",
    session_id: "b",
    text: "from b",
  });

  // Adding session B leaves session A's object identical (same reference).
  assert.equal(states.a, sessionAfterA, "session A is untouched by B's event");
  assert.equal((states.a.messages[0] as ChatMessage).text, "from a");
  assert.equal((states.b.messages[0] as ChatMessage).text, "from b");
});

test("interleaved runs keep independent transcripts and running flags", async () => {
  const { reduceSessions } = await loadSessionRuntime();

  // Two sessions run concurrently, their events arriving interleaved.
  const events = [
    { type: "run.started", session_id: "a" },
    { type: "run.started", session_id: "b" },
    { type: "message.completed", session_id: "a", text: "answer a" },
    {
      type: "tool.started",
      session_id: "b",
      tool_call_id: "b1",
      tool_name: "bash",
    },
    { type: "run.completed", session_id: "a" },
    {
      type: "tool.completed",
      session_id: "b",
      tool_call_id: "b1",
      tool_name: "bash",
      text: "done",
    },
    { type: "message.completed", session_id: "b", text: "answer b" },
  ];

  let states: Record<string, SessionState> = {};
  for (const event of events) {
    states = reduceSessions(states, event);
  }

  // Session A finished with just its own assistant reply.
  assert.equal(states.a.running, false);
  assert.deepEqual(
    states.a.messages.map((message) => (message as ChatMessage).text),
    ["answer a"],
  );

  // Session B is still running and carries only its own tool line + reply.
  assert.equal(states.b.running, true);
  assert.deepEqual(
    states.b.messages.map((message) => message.role),
    ["tool", "assistant"],
  );
  assert.equal((states.b.messages[0] as ToolMessage).state, "done");
  assert.equal((states.b.messages[1] as ChatMessage).text, "answer b");
});

test("events without a session id are ignored", async () => {
  const { reduceSessions, createSessionState } = await loadSessionRuntime();

  const states = { a: createSessionState() };
  assert.equal(
    reduceSessions(states, { type: "user.message", text: "orphan" }),
    states,
    "no session id returns the same map",
  );
});

test("an unrecognized event does not create a phantom session", async () => {
  const { reduceSessions } = await loadSessionRuntime();

  const states = {};
  const next = reduceSessions(states, {
    type: "unknown.event",
    session_id: "a",
  });

  assert.equal(next, states, "no-op event returns the same map");
  assert.deepEqual(Object.keys(next), [], "no empty session was inserted");
});

test("terminal run event helper matches the renderer's set", async () => {
  const { isTerminalRunEvent } = await loadSessionRuntime();

  assert.equal(isTerminalRunEvent("run.completed"), true);
  assert.equal(isTerminalRunEvent("run.cancelled"), true);
  assert.equal(isTerminalRunEvent("run.failed"), true);
  assert.equal(isTerminalRunEvent("message.completed"), false);
});
