import type { ChatMessage } from "../desktop/electron/renderer/src/types.ts";

const assert = require("node:assert/strict");
const { afterEach, test } = require("node:test");

async function loadSessionStore() {
  return import("../desktop/electron/renderer/src/lib/session-store.ts");
}

afterEach(async () => {
  const { resetSessionStateStoreForTests } = await loadSessionStore();
  resetSessionStateStoreForTests();
});

test("session store reduces live events into named session state", async () => {
  const { applyStoredSessionEvent, sessionStateStore } =
    await loadSessionStore();

  applyStoredSessionEvent({ type: "run.started", session_id: "a" });
  applyStoredSessionEvent({
    type: "message.completed",
    session_id: "a",
    text: "answer a",
  });

  const state = sessionStateStore.get();
  assert.equal(state.a.running, true);
  assert.equal((state.a.messages[0] as ChatMessage).text, "answer a");
});

test("session store preserves reducer isolation between sessions", async () => {
  const { applyStoredSessionEvent, sessionStateStore } =
    await loadSessionStore();

  applyStoredSessionEvent({
    type: "user.message",
    session_id: "a",
    text: "from a",
  });
  const sessionA = sessionStateStore.get().a;

  applyStoredSessionEvent({
    type: "user.message",
    session_id: "b",
    text: "from b",
  });

  const state = sessionStateStore.get();
  assert.equal(state.a, sessionA);
  assert.equal((state.b.messages[0] as ChatMessage).text, "from b");
});

test("session store helpers append status messages and ignore missing ids", async () => {
  const {
    appendStoredSessionMessage,
    sessionStateStore,
    updateStoredSessionState,
  } = await loadSessionStore();

  updateStoredSessionState(null, (state) => ({ ...state, running: true }));
  assert.deepEqual(sessionStateStore.get(), {});

  appendStoredSessionMessage("system", "error", "backend unavailable");

  const [message] = sessionStateStore.get().system.messages as ChatMessage[];
  assert.equal(message.role, "error");
  assert.equal(message.text, "backend unavailable");
});
