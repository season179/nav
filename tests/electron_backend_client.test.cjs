const assert = require("node:assert/strict");
const { test } = require("node:test");

const { subscribeToSessionEvents } = require("../desktop/electron/backend-client.cjs");
const { startLocalBackend } = require("../desktop/electron/backend-process.cjs");

const FIXTURE_SESSION_ID = "019f2f6f-f178-7a72-9f28-000000000100";

test("Electron backend client streams fixture session events over HTTP/SSE", async () => {
  const backend = await startFixtureBackend();
  const controller = new AbortController();
  const events = [];

  try {
    await new Promise((resolve, reject) => {
      subscribeToSessionEvents({
        backendUrl: backend.url,
        sessionId: FIXTURE_SESSION_ID,
        signal: controller.signal,
        onEvent(event) {
          events.push(event);
          if (event.type === "run.completed") {
            resolve();
          }
        },
        onError: reject,
      });
    });
  } finally {
    controller.abort();
    backend.stop();
  }

  assert.deepEqual(
    events.map((event) => event.type),
    [
      "session.created",
      "run.started",
      "message.delta",
      "message.completed",
      "run.completed",
    ],
  );
  assert.equal(events[0].session_id, FIXTURE_SESSION_ID);
  assert.equal(
    events[2].text,
    "Hello from the deterministic nav local backend fixture.",
  );
});

async function startFixtureBackend() {
  const backend = await startLocalBackend({
    projectRoot: process.cwd(),
    startupAttempts: 40,
  });

  return {
    url: backend.url,
    stop() {
      backend.child.kill();
    },
  };
}
