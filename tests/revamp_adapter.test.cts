import type { Message } from "../desktop/electron/renderer/src/types.ts";

const assert = require("node:assert/strict");
const { test } = require("node:test");

async function loadAdapter() {
  return import("../desktop/electron/renderer/src/lib/ai-elements-adapter.ts");
}

test("adapter maps user, assistant, and error chat messages", async () => {
  const { adaptMessagesForAiElements } = await loadAdapter();

  const messages: Message[] = [
    {
      id: "user-1",
      role: "user",
      text: "hello",
      createdAt: "2026-06-25T01:00:00.000Z",
    },
    {
      id: "assistant-1",
      role: "assistant",
      text: "hi",
      createdAt: "2026-06-25T01:00:01.000Z",
    },
    {
      id: "error-1",
      role: "error",
      text: "backend unavailable",
      createdAt: "2026-06-25T01:00:02.000Z",
    },
  ];

  assert.deepEqual(adaptMessagesForAiElements(messages), [
    {
      kind: "message",
      id: "user-1",
      role: "user",
      from: "user",
      text: "hello",
      createdAt: "2026-06-25T01:00:00.000Z",
    },
    {
      kind: "message",
      id: "assistant-1",
      role: "assistant",
      from: "assistant",
      text: "hi",
      createdAt: "2026-06-25T01:00:01.000Z",
    },
    {
      kind: "message",
      id: "error-1",
      role: "error",
      from: "assistant",
      text: "backend unavailable",
      createdAt: "2026-06-25T01:00:02.000Z",
    },
  ]);
});

test("adapter preserves tool messages until AI Elements tool states are known", async () => {
  const { adaptMessagesForAiElements } = await loadAdapter();

  assert.deepEqual(
    adaptMessagesForAiElements([
      {
        id: "tool-1",
        role: "tool",
        toolCallId: "call-1",
        state: "running",
        toolName: "read",
        detail: "Reading file",
      },
    ]),
    [
      {
        kind: "tool",
        id: "tool-1",
        toolCallId: "call-1",
        toolName: "read",
        state: "running",
        detail: "Reading file",
      },
    ],
  );
});
