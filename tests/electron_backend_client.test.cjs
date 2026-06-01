const assert = require("node:assert/strict");
const { test } = require("node:test");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const fs = require("node:fs");
const crypto = require("node:crypto");

const {
  subscribeToSessionEvents,
  sendRpc,
} = require("../desktop/electron/backend-client.cjs");
const {
  buildStartupTimeoutMessage,
  collectStderrLines,
  startLocalBackend,
} = require("../desktop/electron/backend-process.cjs");

test("Electron backend client runs a multi-turn chat over RPC + SSE", async () => {
  const backend = await startMockBackend();
  const controller = new AbortController();
  const events = [];
  let projectRoot = null;

  try {
    const created = await sendRpc({
      backendUrl: backend.url,
      method: "session.create",
    });
    const sessionId = created.result.sessionId;
    assert.ok(sessionId, "session.create returns a sessionId");
    const listed = await sendRpc({
      backendUrl: backend.url,
      method: "session.list",
    });
    assert.equal(
      listed.result.sessions[0].workspaceRoot,
      process.cwd(),
      "session.list includes the backend project root",
    );
    projectRoot = fs.mkdtempSync(path.join(os.tmpdir(), "nav-project-"));
    const projectSession = await sendRpc({
      backendUrl: backend.url,
      method: "session.create",
      params: { cwd: projectRoot },
    });
    assert.ok(
      projectSession.result.sessionId,
      "session.create accepts a project cwd",
    );
    const listedWithProject = await sendRpc({
      backendUrl: backend.url,
      method: "session.list",
    });
    const listedProjectSession = listedWithProject.result.sessions.find(
      (session) => session.sessionId === projectSession.result.sessionId,
    );
    assert.ok(
      listedProjectSession,
      "session.list returns the newly created project session",
    );
    assert.equal(
      listedProjectSession.workspaceRoot,
      fs.realpathSync(projectRoot),
      "new project sessions list under their selected directory",
    );

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
    if (projectRoot) {
      fs.rmSync(projectRoot, { recursive: true, force: true });
    }
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

test("SSE subscription reports non-200 response status before failing", async () => {
  const server = http.createServer((_request, response) => {
    response.writeHead(503);
    response.end("unavailable");
  });
  await listen(server);
  const { port } = server.address();
  const opened = [];

  try {
    await assert.rejects(
      new Promise((resolve, reject) => {
        subscribeToSessionEvents({
          backendUrl: `http://127.0.0.1:${port}`,
          sessionId: "session-id",
          onEvent: resolve,
          onError: reject,
          onOpen(event) {
            opened.push(event);
          },
        });
      }),
      /SSE request failed with HTTP 503/,
    );
  } finally {
    await close(server);
  }

  assert.deepEqual(opened, [{ statusCode: 503 }]);
});

test("backend stderr collector preserves split trace lines", () => {
  const lines = [];
  const remainder = collectStderrLines({
    chunk: 'nav startup trace {"event":"backend.',
    onLine(line) {
      lines.push(line);
    },
  });
  const finalRemainder = collectStderrLines({
    chunk: 'ready"}\nnav-local-backend: using mock\npartial',
    previousRemainder: remainder,
    onLine(line) {
      lines.push(line);
    },
  });

  assert.deepEqual(lines, [
    'nav startup trace {"event":"backend.ready"}',
    "nav-local-backend: using mock",
  ]);
  assert.equal(finalRemainder, "partial");
});

test("backend startup timeout reports duration and captured output", () => {
  const message = buildStartupTimeoutMessage({
    startupAttempts: 3,
    stderr: Array.from(
      { length: 14 },
      (_value, index) => `stderr ${index + 1}`,
    ).join("\n"),
    stdout: "cargo build still running",
  });

  assert.match(message, /backend did not print a local URL within 0.2s/);
  assert.match(message, /backend stderr:\nstderr 3/);
  assert.doesNotMatch(message, /^stderr 1$/m);
  assert.match(message, /stderr 14/);
  assert.match(message, /backend stdout:\ncargo build still running/);
});

async function startMockBackend() {
  // Persist to a throwaway database so the test never touches ~/.nav/nav.db.
  const dbPath = path.join(
    os.tmpdir(),
    `nav-electron-test-${crypto.randomUUID()}.db`,
  );
  const backend = await startLocalBackend({
    projectRoot: process.cwd(),
    startupAttempts: 80,
    env: { NAV_MOCK_MODEL: "1", NAV_DB_PATH: dbPath },
  });

  return {
    url: backend.url,
    stop() {
      backend.child.kill();
      for (const suffix of ["", "-wal", "-shm"]) {
        fs.rmSync(`${dbPath}${suffix}`, { force: true });
      }
    },
  };
}

function listen(server) {
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", resolve);
  });
}

function close(server) {
  return new Promise((resolve, reject) => {
    server.close((error) => {
      if (error) {
        reject(error);
        return;
      }
      resolve();
    });
  });
}
