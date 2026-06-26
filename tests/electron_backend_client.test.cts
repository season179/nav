import type { IncomingMessage, Server, ServerResponse } from "node:http";

const assert = require("node:assert/strict");
const childProcess = require("node:child_process");
const { test } = require("node:test");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const fs = require("node:fs");
const crypto = require("node:crypto");
const { once } = require("node:events");

const {
  subscribeToSessionEvents,
  sendRpc,
} = require("../desktop/electron/out/backend-client.cjs");
const {
  buildStartupTimeoutMessage,
  collectStderrLines,
  startLocalBackend,
} = require("../desktop/electron/out/backend-process.cjs");

test("Electron backend client reaches the Flue control plane", async () => {
  const backend = await startIsolatedBackend();
  let projectRoot = null;

  try {
    const healthResponse = await fetch(new URL("/health", backend.url));
    assert.equal(healthResponse.status, 200);
    assert.equal((await healthResponse.json()).service, "nav-backend");

    const created = await sendRpc({
      backendUrl: backend.url,
      method: "session.create",
      params: { mode: "local" },
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
      (session: { sessionId: string }) =>
        session.sessionId === projectSession.result.sessionId,
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

    const latest = await sendRpc({
      backendUrl: backend.url,
      method: "session.latest",
      params: { cwd: projectRoot },
    });
    assert.equal(latest.result.sessionId, projectSession.result.sessionId);

    const resumed = await sendRpc({
      backendUrl: backend.url,
      method: "session.resume",
      params: { sessionId },
    });
    assert.equal(resumed.result.sessionId, sessionId);

    const stopped = await sendRpc({
      backendUrl: backend.url,
      method: "session.stop",
      params: { sessionId },
    });
    assert.equal(stopped.result.stopped, false);

    const stacks = await sendRpc({
      backendUrl: backend.url,
      method: "session.stacks",
      params: { sessionId },
    });
    assert.deepEqual(stacks.result.stacks, []);

    const availability = await sendRpc({
      backendUrl: backend.url,
      method: "session.stackAvailability",
      params: { sessionId },
    });
    assert.equal(availability.result.available, false);
  } finally {
    await backend.stop();
    if (projectRoot) {
      fs.rmSync(projectRoot, { recursive: true, force: true });
    }
  }
});

test("Electron backend client lists and switches configured models", async () => {
  const settingsDir = fs.mkdtempSync(path.join(os.tmpdir(), "nav-settings-"));
  const settingsPath = path.join(settingsDir, "settings.json");
  const codexAuthPath = path.join(settingsDir, "codex-auth.json");
  fs.writeFileSync(
    settingsPath,
    JSON.stringify({
      defaultModel: { provider: "codex", model: "gpt-5.5" },
      providers: {
        codex: {
          api: "codex-responses",
          models: [
            {
              id: "gpt-5.5",
              name: "GPT 5.5",
              reasoning: true,
              contextWindow: 272_000,
            },
          ],
        },
        commandcode: {
          api: "openai-completions",
          apiKey: "COMMANDCODE_KEY",
          baseUrl: "https://api.commandcode.example/provider/v1",
          models: [
            {
              id: "Qwen/Qwen3.7-Max",
              name: "Qwen 3.7 Max",
              reasoning: false,
              contextWindow: 1_000_000,
            },
          ],
        },
      },
    }),
  );
  fs.writeFileSync(
    codexAuthPath,
    JSON.stringify({ tokens: { access_token: "codex-access-token" } }),
  );
  const backend = await startIsolatedBackend({
    COMMANDCODE_KEY: "secret-value",
    NAV_CODEX_AUTH_PATH: codexAuthPath,
    NAV_SETTINGS_PATH: settingsPath,
  });

  try {
    const listed = await sendRpc({
      backendUrl: backend.url,
      method: "session.models",
    });
    assert.deepEqual(listed.result.models, [
      {
        provider: "openai-codex",
        model: "gpt-5.5",
        label: "GPT 5.5",
        thinkingLevels: ["off", "minimal", "low", "medium", "high", "xhigh"],
      },
      {
        provider: "commandcode",
        model: "Qwen/Qwen3.7-Max",
        label: "Qwen 3.7 Max",
        thinkingLevels: ["off"],
      },
    ]);
    const created = await sendRpc({
      backendUrl: backend.url,
      method: "session.create",
    });
    const sessionId = created.result.sessionId;
    assert.ok(sessionId, "session.create returns a sessionId");

    const before = await sendRpc({
      backendUrl: backend.url,
      method: "session.modelInfo",
      params: { sessionId },
    });
    assert.equal(before.result.label, "GPT 5.5");
    assert.equal(before.result.provider, "openai-codex");
    assert.equal(before.result.model, "gpt-5.5");
    assert.equal(before.result.thinking, "medium");
    assert.deepEqual(before.result.thinkingLevels, [
      "off",
      "minimal",
      "low",
      "medium",
      "high",
      "xhigh",
    ]);

    const switched = await sendRpc({
      backendUrl: backend.url,
      method: "session.switchModel",
      params: {
        sessionId,
        provider: "commandcode",
        model: "Qwen/Qwen3.7-Max",
      },
    });
    assert.equal(switched.result.modelInfo.label, "Qwen 3.7 Max");
    assert.equal(switched.result.modelInfo.provider, "commandcode");
    assert.equal(switched.result.modelInfo.model, "Qwen/Qwen3.7-Max");
    assert.equal(switched.result.modelInfo.thinking, "off");
    assert.deepEqual(switched.result.modelInfo.thinkingLevels, ["off"]);

    const thinkingOff = await sendRpc({
      backendUrl: backend.url,
      method: "session.switchThinking",
      params: { sessionId, thinkingLevel: "off" },
    });
    assert.equal(thinkingOff.result.modelInfo.label, "Qwen 3.7 Max");
    assert.equal(thinkingOff.result.modelInfo.thinking, "off");

    const thinkingHigh = await sendRpc({
      backendUrl: backend.url,
      method: "session.switchThinking",
      params: { sessionId, thinkingLevel: "xhigh" },
    });
    assert.equal(thinkingHigh.result.modelInfo.label, "Qwen 3.7 Max");
    assert.equal(thinkingHigh.result.modelInfo.thinking, "off");

    const after = await sendRpc({
      backendUrl: backend.url,
      method: "session.modelInfo",
      params: { sessionId },
    });
    assert.equal(after.result.label, "Qwen 3.7 Max");
    assert.equal(after.result.provider, "commandcode");
    assert.equal(after.result.model, "Qwen/Qwen3.7-Max");
    assert.equal(after.result.thinking, "off");

    // The switch is scoped to this session: the default new sessions start
    // from is untouched.
    const untouchedDefault = await sendRpc({
      backendUrl: backend.url,
      method: "session.modelInfo",
    });
    assert.equal(untouchedDefault.result.label, "GPT 5.5");
    assert.equal(untouchedDefault.result.model, "gpt-5.5");
  } finally {
    await backend.stop();
    fs.rmSync(settingsDir, { recursive: true, force: true });
  }
});

test("SSE subscription reports non-200 response status before failing", async () => {
  const server = http.createServer(
    (_request: IncomingMessage, response: ServerResponse) => {
      response.writeHead(503);
      response.end("unavailable");
    },
  );
  await listen(server);
  const { port } = server.address();
  const opened: { statusCode: number }[] = [];

  try {
    await assert.rejects(
      new Promise((resolve, reject) => {
        subscribeToSessionEvents({
          backendUrl: `http://127.0.0.1:${port}`,
          sessionId: "session-id",
          onEvent: resolve,
          onError: reject,
          onOpen(event: { statusCode: number }) {
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
  const lines: string[] = [];
  const remainder = collectStderrLines({
    chunk: 'nav startup trace {"event":"backend.',
    onLine(line: string) {
      lines.push(line);
    },
  });
  const finalRemainder = collectStderrLines({
    chunk: 'ready"}\nflue backend: using mock\npartial',
    previousRemainder: remainder,
    onLine(line: string) {
      lines.push(line);
    },
  });

  assert.deepEqual(lines, [
    'nav startup trace {"event":"backend.ready"}',
    "flue backend: using mock",
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
    stdout: "flue build still running",
  });

  assert.match(message, /backend did not print a local URL within 0.2s/);
  assert.match(message, /backend stderr:\nstderr 3/);
  assert.doesNotMatch(message, /^stderr 1$/m);
  assert.match(message, /stderr 14/);
  assert.match(message, /backend stdout:\nflue build still running/);
});

async function startIsolatedBackend(env = {}) {
  // Persist to throwaway files so the test never touches backend/data.
  const dataDir = fs.mkdtempSync(path.join(os.tmpdir(), "nav-backend-"));
  const sessionCatalogPath = path.join(dataDir, "sessions.json");
  const stacksPath = path.join(dataDir, `stacks-${crypto.randomUUID()}.json`);
  const backend = await startLocalBackend({
    projectRoot: process.cwd(),
    startupAttempts: 80,
    env: {
      NAV_SESSION_CATALOG_PATH: sessionCatalogPath,
      NAV_CODEX_AUTH_PATH: path.join(dataDir, "missing-codex-auth.json"),
      NAV_SETTINGS_PATH: path.join(dataDir, "missing-settings.json"),
      NAV_STACKS_PATH: stacksPath,
      ...env,
    },
  });

  return {
    url: backend.url,
    async stop() {
      if (backend.child.exitCode === null) {
        backend.child.kill();
        await once(backend.child, "exit");
      }
      fs.rmSync(dataDir, { recursive: true, force: true });
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

function listen(server: Server) {
  return new Promise<void>((resolve) => {
    server.listen(0, "127.0.0.1", resolve);
  });
}

function close(server: Server) {
  return new Promise<void>((resolve, reject) => {
    server.close((error) => {
      if (error) {
        reject(error);
        return;
      }
      resolve();
    });
  });
}
