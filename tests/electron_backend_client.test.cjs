const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
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
      "session.list includes the backend workspace root",
    );
    assert.equal(
      listed.result.sessions[0].projectRoot,
      currentRepoMainCheckout(),
      "session.list includes the backend sidebar project root",
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

test("Electron backend client lists and switches configured models", async () => {
  const backend = await startConfiguredBackend();

  try {
    const listed = await sendRpc({
      backendUrl: backend.url,
      method: "session.models",
    });
    assert.deepEqual(listed.result.models, [
      {
        provider: "local",
        model: "qwen-coder",
        label: "Qwen Coder",
      },
      {
        provider: "openai",
        model: "gpt-default",
        label: "Default GPT",
      },
    ]);

    const before = await sendRpc({
      backendUrl: backend.url,
      method: "session.modelInfo",
    });
    assert.equal(before.result.label, "Default GPT");
    assert.equal(before.result.provider, "openai");
    assert.equal(before.result.model, "gpt-default");

    const switched = await sendRpc({
      backendUrl: backend.url,
      method: "session.switchModel",
      params: { provider: "local", model: "qwen-coder" },
    });
    assert.equal(switched.result.modelInfo.label, "Qwen Coder");
    assert.equal(switched.result.modelInfo.provider, "local");
    assert.equal(switched.result.modelInfo.model, "qwen-coder");

    const after = await sendRpc({
      backendUrl: backend.url,
      method: "session.modelInfo",
    });
    assert.equal(after.result.label, "Qwen Coder");
    assert.equal(after.result.provider, "local");
    assert.equal(after.result.model, "qwen-coder");
  } finally {
    backend.stop();
  }
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

async function startConfiguredBackend() {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "nav-home-"));
  const navDir = path.join(home, ".nav");
  fs.mkdirSync(navDir, { recursive: true });
  fs.writeFileSync(
    path.join(navDir, "settings.json"),
    JSON.stringify({
      defaultModel: {
        provider: "openai",
        model: "gpt-default",
      },
      providers: {
        openai: {
          baseUrl: "https://api.openai.example/v1",
          apiKey: "test-openai-key",
          api: "openai-completions",
          models: [
            {
              id: "gpt-default",
              name: "Default GPT",
            },
          ],
        },
        local: {
          baseUrl: "http://localhost:11434/v1",
          apiKey: "test-local-key",
          api: "openai-completions",
          models: [
            {
              id: "qwen-coder",
              name: "Qwen Coder",
            },
          ],
        },
      },
    }),
  );

  const dbPath = path.join(
    os.tmpdir(),
    `nav-electron-config-test-${crypto.randomUUID()}.db`,
  );
  const backend = await startLocalBackend({
    projectRoot: process.cwd(),
    startupAttempts: 80,
    env: {
      HOME: home,
      CARGO_HOME: process.env.CARGO_HOME ?? path.join(os.homedir(), ".cargo"),
      RUSTUP_HOME:
        process.env.RUSTUP_HOME ?? path.join(os.homedir(), ".rustup"),
      NAV_DB_PATH: dbPath,
      NAV_MOCK_MODEL: "",
    },
  });

  return {
    url: backend.url,
    stop() {
      backend.child.kill();
      fs.rmSync(home, { recursive: true, force: true });
      for (const suffix of ["", "-wal", "-shm"]) {
        fs.rmSync(`${dbPath}${suffix}`, { force: true });
      }
    },
  };
}

function currentRepoMainCheckout() {
  const commonGitDir = childProcess
    .execFileSync("git", ["rev-parse", "--git-common-dir"], {
      cwd: process.cwd(),
      encoding: "utf8",
    })
    .trim();
  const resolvedCommonGitDir = path.resolve(process.cwd(), commonGitDir);
  return path.basename(resolvedCommonGitDir) === ".git"
    ? fs.realpathSync(path.dirname(resolvedCommonGitDir))
    : process.cwd();
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
