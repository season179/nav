import type {
  ChatMessage,
  SessionEvent,
  SessionState,
  ToolMessage,
} from "../desktop/electron/renderer/src/types.ts";

const assert = require("node:assert/strict");
const { test } = require("node:test");

const {
  flueEventsToSessionEvents,
  parseFlueSseBuffer,
} = require("../desktop/electron/out/flue-session-events.cjs");

function loadSessionRuntime() {
  return import("../desktop/electron/renderer/src/lib/session-runtime.ts");
}

test("Flue SSE parser reads data arrays, control frames, and heartbeats", () => {
  const parsed = parseFlueSseBuffer(
    [
      ": heartbeat",
      "",
      "event: data",
      'data: [{"v":3,"type":"agent_start","eventIndex":1}]',
      "",
      "event: control",
      'data: {"streamNextOffset":"0000000000000000_0000000000000001","upToDate":true}',
      "",
      "event: data",
      "data: not-json",
      "",
      "event: data",
      'data: [{"v":3,"type":"idle","eventIndex":2}]',
      "",
      "",
    ].join("\n"),
    { sessionId: "session-1" },
  );

  assert.deepEqual(
    parsed.events.map((event: SessionEvent) => event.type),
    ["run.started", "run.completed"],
  );
  assert.equal(parsed.events[0].session_id, "session-1");
  assert.equal(parsed.events[0].event_id, "flue:session-1:1:run.started");
  assert.equal(
    parsed.controls[0].streamNextOffset,
    "0000000000000000_0000000000000001",
  );
  assert.equal(parsed.controls[0].upToDate, true);
  assert.equal(parsed.malformedFrames, 1);
});

test("Flue SSE parser preserves an incomplete frame as the next remainder", () => {
  const parsed = parseFlueSseBuffer(
    'event: data\ndata: [{"v":3,"type":"agent_start","eventIndex":1}]\n\n' +
      'event: data\ndata: [{"v":3,"type":"idle"',
    { sessionId: "session-1" },
  );

  assert.deepEqual(
    parsed.events.map((event: SessionEvent) => event.type),
    ["run.started"],
  );
  assert.match(parsed.remainder, /"idle"/);
});

test("Flue transcript events reduce to live text and authoritative message end", async () => {
  const { reduceSessions } = await loadSessionRuntime();
  const events = flueEventsToSessionEvents([
    { v: 3, type: "agent_start", instanceId: "session-1", eventIndex: 1 },
    {
      v: 3,
      type: "message_end",
      instanceId: "session-1",
      eventIndex: 2,
      message: { role: "user", content: "hello" },
    },
    {
      v: 3,
      type: "text_delta",
      instanceId: "session-1",
      eventIndex: 3,
      text: "hel",
    },
    {
      v: 3,
      type: "text_delta",
      instanceId: "session-1",
      eventIndex: 4,
      delta: "lo",
    },
    {
      v: 3,
      type: "message_end",
      instanceId: "session-1",
      eventIndex: 5,
      message: {
        role: "assistant",
        content: [{ type: "text", text: "hello!" }],
      },
    },
    { v: 3, type: "idle", instanceId: "session-1", eventIndex: 6 },
  ]);

  let states: Record<string, SessionState> = {};
  for (const event of events) {
    states = reduceSessions(states, event);
  }

  assert.deepEqual(
    states["session-1"].messages.map((message: ChatMessage | ToolMessage) => [
      message.role,
      message.role === "tool" ? message.detail : message.text,
    ]),
    [
      ["user", "hello"],
      ["assistant", "hello!"],
    ],
  );
  assert.equal(states["session-1"].running, false);
});

test("Flue tool events reduce ok and failed tool lifecycles by call id", async () => {
  const { reduceSessions } = await loadSessionRuntime();
  const events = flueEventsToSessionEvents(
    [
      {
        v: 3,
        type: "tool_start",
        eventIndex: 1,
        toolCallId: "call-ok",
        toolName: "bash",
      },
      {
        v: 3,
        type: "tool",
        eventIndex: 2,
        toolCallId: "call-ok",
        toolName: "bash",
        result: "listed files",
      },
      {
        v: 3,
        type: "tool_start",
        eventIndex: 3,
        toolCallId: "call-bad",
        toolName: "read",
      },
      {
        v: 3,
        type: "tool",
        eventIndex: 4,
        toolCallId: "call-bad",
        toolName: "read",
        isError: true,
        error: { message: "missing file" },
      },
    ],
    { sessionId: "session-1" },
  );

  let states: Record<string, SessionState> = {};
  for (const event of events) {
    states = reduceSessions(states, event);
  }

  const [okTool, failedTool] = states["session-1"].messages as ToolMessage[];
  assert.deepEqual(
    [okTool.state, okTool.toolCallId, okTool.toolName, okTool.detail],
    ["done", "call-ok", "bash", "listed files"],
  );
  assert.deepEqual(
    [
      failedTool.state,
      failedTool.toolCallId,
      failedTool.toolName,
      failedTool.detail,
    ],
    ["failed", "call-bad", "read", "missing file"],
  );
});

test("Flue terminal error maps to one failed run even when idle follows", async () => {
  const { reduceSessions } = await loadSessionRuntime();
  const events = flueEventsToSessionEvents(
    [
      { v: 3, type: "agent_start", eventIndex: 1 },
      {
        v: 3,
        type: "operation",
        eventIndex: 2,
        isError: true,
        error: "model exploded",
      },
      { v: 3, type: "idle", eventIndex: 3 },
    ],
    { sessionId: "session-1" },
  );

  assert.deepEqual(
    events.map((event: SessionEvent) => event.type),
    ["run.started", "run.failed"],
  );

  let states: Record<string, SessionState> = {};
  for (const event of events) {
    states = reduceSessions(states, event);
  }

  const lastMessage = states["session-1"].messages.at(-1) as
    | ChatMessage
    | undefined;
  assert.equal(states["session-1"].running, false);
  assert.equal(lastMessage?.role, "error");
  assert.equal(lastMessage?.text, "model exploded");
});
