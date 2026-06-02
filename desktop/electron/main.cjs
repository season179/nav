const { app, BrowserWindow, dialog, ipcMain } = require("electron");
const fs = require("node:fs/promises");
const path = require("node:path");
const { subscribeToSessionEvents, sendRpc } = require("./backend-client.cjs");
const { startLocalBackend } = require("./backend-process.cjs");
const {
  existingProjectSessionId,
  normalizeWorkspaceRoot,
} = require("./project-session.cjs");
const { createStartupTrace } = require("./startup-trace.cjs");
const { createWindowOptions } = require("./window-options.cjs");

const PROJECT_ROOT = path.resolve(__dirname, "../..");
const smokeMode = process.argv.includes("--smoke");
const trace = createStartupTrace();
trace.mark("electron.main.loaded", { smoke: smokeMode });

let backendProcess = null;
let eventSubscription = null;
let backendUrl = null;
let sessionId = null;
let mainWindow = null;
let firstSessionEventSeen = false;
const pendingProjectSessions = new Map();

app.whenReady().then(async () => {
  trace.mark("electron.app.ready");
  mainWindow = createMainWindow();
  await startChatSession(mainWindow);
});

app.on("before-quit", stopBackend);
app.on("window-all-closed", () => {
  stopBackend();
  app.quit();
});

// The renderer can only ask Main to send a chat message; Main owns all backend
// transport.
ipcMain.handle("nav:send-message", async (_event, text) => {
  if (!backendUrl || !sessionId) {
    throw new Error("chat session is not ready");
  }
  await sendRpc({
    backendUrl,
    method: "session.sendMessage",
    params: { sessionId, text },
  });
});

ipcMain.handle("nav:stop", async () => {
  if (!backendUrl || !sessionId) {
    throw new Error("chat session is not ready");
  }
  const response = await sendRpc({
    backendUrl,
    method: "session.stop",
    params: { sessionId },
  });
  return response.result.stopped === true;
});

ipcMain.handle("nav:list-sessions", async () => {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  const response = await sendRpc({ backendUrl, method: "session.list" });
  return response.result.sessions;
});

ipcMain.handle("nav:model-info", async (_event, requestedSessionId) => {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  const response = await sendRpc({
    backendUrl,
    method: "session.modelInfo",
    params: requestedSessionId ? { sessionId: requestedSessionId } : undefined,
  });
  return response.result;
});

ipcMain.handle("nav:model-list", async () => {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  const response = await sendRpc({
    backendUrl,
    method: "session.models",
  });
  return response.result.models;
});

ipcMain.handle("nav:switch-model", async (_event, request) => {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  const response = await sendRpc({
    backendUrl,
    method: "session.switchModel",
    params: request,
  });
  return response.result.modelInfo;
});

ipcMain.handle("nav:switch-thinking", async (_event, thinkingLevel) => {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  const response = await sendRpc({
    backendUrl,
    method: "session.switchThinking",
    params: { thinkingLevel },
  });
  return response.result.modelInfo;
});

ipcMain.handle("nav:session-stacks", async (_event, requestedSessionId) => {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  const targetSessionId = requestedSessionId || sessionId;
  if (!targetSessionId) {
    throw new Error("chat session is not ready");
  }
  const response = await sendRpc({
    backendUrl,
    method: "session.stacks",
    params: { sessionId: targetSessionId },
  });
  return response.result;
});

ipcMain.handle(
  "nav:session-stack-availability",
  async (_event, requestedSessionId) => {
    if (!backendUrl) {
      throw new Error("chat session is not ready");
    }
    const targetSessionId = requestedSessionId || sessionId;
    if (!targetSessionId) {
      throw new Error("chat session is not ready");
    }
    const response = await sendRpc({
      backendUrl,
      method: "session.stackAvailability",
      params: { sessionId: targetSessionId },
    });
    return response.result;
  },
);

ipcMain.handle("nav:switch-session", async (_event, id) => {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  await sendRpc({
    backendUrl,
    method: "session.resume",
    params: { sessionId: id },
  });
  activateSession(mainWindow, id);
});

ipcMain.handle("nav:create-project", async (_event, request) => {
  if (!backendUrl || !mainWindow) {
    throw new Error("chat session is not ready");
  }
  const { mode: requestedMode } = normalizeNewSessionRequest(request);
  const selection = await dialog.showOpenDialog(mainWindow, {
    title: "Add Project",
    buttonLabel: "Add Project",
    properties: ["openDirectory"],
  });
  const cwd = selection.canceled ? null : selection.filePaths[0];
  if (!cwd) {
    return null;
  }
  const workspaceRoot = normalizeWorkspaceRoot(await fs.realpath(cwd));
  const projectSessionId = await openOrCreateProjectSession(
    workspaceRoot,
    requestedMode ?? "local",
  );
  if (!projectSessionId) {
    return null;
  }
  activateSession(mainWindow, projectSessionId);
  return projectSessionId;
});

ipcMain.handle("nav:new-session", async (_event, request) => {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  const { cwd, mode: requestedMode } = normalizeNewSessionRequest(request);
  const mode = requestedMode ?? "local";
  const createdSessionId = await createBackendSession(cwd, mode);
  activateSession(mainWindow, createdSessionId);
  return createdSessionId;
});

function createMainWindow() {
  trace.mark("electron.window.create.start");
  const window = new BrowserWindow(
    createWindowOptions({ preloadPath: path.join(__dirname, "preload.cjs") }),
  );
  trace.mark("electron.window.create.end");
  window.webContents.once("dom-ready", () => {
    trace.mark("electron.renderer.dom_ready");
  });
  window.webContents.once("did-finish-load", () => {
    trace.mark("electron.renderer.did_finish_load");
  });
  window.webContents.once("did-fail-load", (_event, code, description) => {
    trace.mark("electron.renderer.did_fail_load", {
      error_code: code,
      error: description,
    });
  });
  trace.mark("electron.window.load_file.start");
  window
    .loadFile(path.join(__dirname, "renderer", "dist", "index.html"))
    .then(() => {
      trace.mark("electron.window.load_file.end");
    })
    .catch((error) => {
      trace.mark("electron.window.load_file.failed", { error: error.message });
    });
  return window;
}

async function startChatSession(window) {
  trace.mark("electron.chat.start");
  sendStatus(window, { state: "starting-backend" });

  try {
    // Tests and offline smoke use the deterministic mock; a real launch
    // inherits the user's environment (NAV_API_KEY, etc.).
    const backend = await startLocalBackend({
      projectRoot: PROJECT_ROOT,
      env: trace.childEnv(smokeMode ? { NAV_MOCK_MODEL: "1" } : {}),
      trace,
    });
    backendProcess = backend.child;
    backendUrl = backend.url;
    trace.mark("electron.backend.ready", { pid: backendProcess.pid });

    activateSession(window, await openSession(), { startup: true });

    if (smokeMode) {
      await runSmokeTurn();
    }
  } catch (error) {
    trace.mark("electron.startup.failed", { error: error.message });
    sendStatus(window, { state: "backend-error", message: error.message });
    if (smokeMode) {
      console.error(error);
      printStartupSummary();
      app.exit(1);
    }
  }
}

// Make `id` the active conversation: point the event stream at it (replacing any
// prior subscription) and tell the renderer it's connected. The session's
// backlog replays over the stream, so switching redraws the transcript.
function activateSession(window, id, { startup = false } = {}) {
  sessionId = id;
  eventSubscription?.close();
  markStartup(startup, "electron.sse.subscribe.start", { session_id: id });
  eventSubscription = subscribeToSessionEvents({
    backendUrl,
    sessionId: id,
    onOpen({ statusCode } = {}) {
      markStartup(startup, "electron.sse.open", {
        session_id: id,
        status_code: statusCode,
      });
      if (statusCode !== 200) {
        return;
      }
      // Smoke mode drives the turn from Main and quits on `run.completed`;
      // telling the renderer it is connected can start sidebar/model refreshes
      // that race the intentional shutdown.
      if (!smokeMode) {
        sendStatus(window, { state: "connected", backendUrl, sessionId: id });
      }
      markStartup(startup, "electron.connected", { session_id: id });
    },
    onEvent(event) {
      if (startup) {
        markFirstStartupEvent(id, event);
      }
      forwardEvent(window, event);
    },
    onError(error) {
      markStartup(startup, "electron.sse.error", { error: error.message });
      sendStatus(window, { state: "stream-error", message: error.message });
      if (smokeMode) {
        console.error(error);
        printStartupSummary();
        app.exit(1);
      }
    },
  });
}

// Reopen the most recent conversation so sessions persist across launches.
// Smoke mode always starts fresh to stay deterministic.
async function openSession() {
  trace.mark("electron.session.open.start");
  if (!smokeMode) {
    const latest = await tracedRpc("session.latest", { cwd: PROJECT_ROOT });
    if (latest.result?.sessionId) {
      // If resumption fails (pruned session, corrupt DB, schema mismatch), fall
      // back to a fresh session rather than leaving the app stuck on launch.
      try {
        const resumed = await tracedRpc("session.resume", {
          sessionId: latest.result.sessionId,
        });
        trace.mark("electron.session.open.end", { mode: "resumed" });
        return resumed.result.sessionId;
      } catch (error) {
        trace.mark("electron.session.resume.fallback", {
          error: error.message,
        });
        console.error(
          `failed to resume session ${latest.result.sessionId}, starting fresh: ${error.message}`,
        );
      }
    }
  }
  const created = await tracedRpc("session.create", {
    cwd: PROJECT_ROOT,
    mode: "local",
  });
  trace.mark("electron.session.open.end", { mode: "created" });
  return created.result.sessionId;
}

async function createBackendSession(cwd, mode) {
  const params = cwd ? { cwd, mode } : { mode };
  const created = await sendRpc({
    backendUrl,
    method: "session.create",
    params,
  });
  return created.result.sessionId;
}

async function openOrCreateProjectSession(workspaceRoot, mode = "local") {
  // The resolved session depends on both the root and the mode, so dedupe
  // in-flight lookups per (mode, root) — keying on the root alone would let a
  // concurrent local and worktree open of the same path share one promise and
  // return a session of the wrong mode.
  const pendingKey = `${mode} ${workspaceRoot}`;
  const pending = pendingProjectSessions.get(pendingKey);
  if (pending) {
    return pending;
  }

  const lookup = findExistingProjectSession(workspaceRoot, mode).then(
    async (existingSessionId) =>
      existingSessionId ?? createProjectSession(workspaceRoot, mode),
  );
  pendingProjectSessions.set(pendingKey, lookup);

  try {
    return await lookup;
  } finally {
    pendingProjectSessions.delete(pendingKey);
  }
}

async function findExistingProjectSession(workspaceRoot, mode) {
  const response = await sendRpc({
    backendUrl,
    method: "session.list",
  });
  return existingProjectSessionId(
    response.result.sessions,
    workspaceRoot,
    mode,
  );
}

async function createProjectSession(workspaceRoot, mode) {
  return createBackendSession(workspaceRoot, mode);
}

function normalizeNewSessionRequest(request) {
  if (!request || typeof request !== "object") {
    return { cwd: null, mode: null };
  }
  return {
    cwd: normalizeOptionalCwd(request.cwd),
    mode: normalizeOptionalMode(request.mode),
  };
}

function normalizeOptionalCwd(cwd) {
  if (typeof cwd !== "string") {
    return null;
  }
  const trimmed = cwd.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function normalizeOptionalMode(mode) {
  return mode === "local" || mode === "worktree" ? mode : null;
}

function forwardEvent(window, event) {
  if (!window.isDestroyed()) {
    window.webContents.send("nav:session-event", event);
  }
  if (smokeMode && event.type === "run.completed") {
    trace.mark("electron.smoke.run_completed");
    console.log("nav electron smoke received run.completed");
    printStartupSummary();
    app.quit();
  }
  if (smokeMode && event.type === "run.failed") {
    trace.mark("electron.smoke.run_failed", {
      error: event.error ?? "run failed",
    });
    console.error(`nav electron smoke run failed: ${event.error}`);
    printStartupSummary();
    app.exit(1);
  }
}

async function runSmokeTurn() {
  trace.mark("electron.smoke.turn.start");
  await tracedRpc("session.sendMessage", {
    sessionId,
    text: "smoke test message",
  });
  trace.mark("electron.smoke.turn.accepted");
}

function sendStatus(window, status) {
  if (!window.isDestroyed()) {
    window.webContents.send("nav:backend-status", status);
  }
}

function stopBackend() {
  eventSubscription?.close();
  eventSubscription = null;
  backendProcess?.kill();
  backendProcess = null;
}

async function tracedRpc(method, params) {
  trace.mark(`electron.rpc.${method}.start`);
  try {
    const response = await sendRpc({ backendUrl, method, params });
    trace.mark(`electron.rpc.${method}.end`);
    return response;
  } catch (error) {
    trace.mark(`electron.rpc.${method}.failed`, { error: error.message });
    throw error;
  }
}

function printStartupSummary() {
  const summary = trace.summaryLine();
  if (summary) {
    console.log(summary);
  }
}

function markStartup(startup, event, fields) {
  if (startup) {
    trace.mark(event, fields);
  }
}

function markFirstStartupEvent(id, event) {
  if (firstSessionEventSeen) {
    return;
  }
  firstSessionEventSeen = true;
  trace.mark("electron.sse.first_event", {
    session_id: id,
    event_type: event.type,
  });
}
