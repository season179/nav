const assert = require("node:assert/strict");
const { test } = require("node:test");

const {
  subscribeToSessionEvents,
  sendRpc,
} = require("../desktop/electron/backend-client.cjs");
const { startLocalBackend } = require("../desktop/electron/backend-process.cjs");

test("Electron backend client runs a multi-turn chat over RPC + SSE", async () => {
  const backend = await startMockBackend();
  const controller = new AbortController();
  const events = [];

  try {
    const created = await sendRpc({
      backendUrl: backend.url,
      method: "session.create",
    });
    const sessionId = created.result.sessionId;
    assert.ok(sessionId, "session.create returns a sessionId");

    await new Promise((resolve, reject) => {
      let completions = 0;
      subscribeToSessionEvents({
        backendUrl: backend.url,
        sessionId,
        signal: controller.signal,
        onEvent(event) {
          events.push(event);
          if (event.type === "run.completed") {
            completions += 1;
            if (completions === 1) {
              // First turn done; send a follow-up that depends on it.
              sendRpc({
                backendUrl: backend.url,
                method: "session.sendMessage",
                params: { sessionId, text: "what is my name?" },
              }).catch(reject);
            } else {
              resolve();
            }
          }
        },
        onError: reject,
      });

      // First turn.
      sendRpc({
        backendUrl: backend.url,
        method: "session.sendMessage",
        params: { sessionId, text: "my name is Ada" },
      }).catch(reject);
    });
  } finally {
    controller.abort();
    backend.stop();
  }

  const types = events.map((event) => event.type);
  assert.deepEqual(types, [
    "session.created",
    "user.message",
    "run.started",
    "message.completed",
    "run.completed",
    "user.message",
    "run.started",
    "message.completed",
    "run.completed",
  ]);

  const assistantReplies = events.filter(
    (event) => event.type === "message.completed",
  );
  // The second reply proves prior context was forwarded to the model.
  assert.match(assistantReplies[1].text, /what is my name\?/);
  assert.match(assistantReplies[1].text, /my name is Ada/);
});

async function startMockBackend() {
  const backend = await startLocalBackend({
    projectRoot: process.cwd(),
    startupAttempts: 80,
    env: { NAV_MOCK_MODEL: "1" },
  });

  return {
    url: backend.url,
    stop() {
      backend.child.kill();
    },
  };
}
