import type { ChildProcess } from "node:child_process";
import fs from "node:fs/promises";
import path from "node:path";
import { app, BrowserWindow, dialog, ipcMain, shell } from "electron";
import type { Subscription } from "./backend-client.cjs";
import { sendRpc, subscribeToSessionEvents } from "./backend-client.cjs";
import { startLocalBackend } from "./backend-process.cjs";
import { isExternalBrowserUrl } from "./external-links.cjs";
import {
  existingProjectSessionId,
  normalizeWorkspaceRoot,
} from "./project-session.cjs";
import type { SessionMode } from "./request-validation.cjs";
import { readSessionMode, writeSessionMode } from "./session-mode-store.cjs";
import { createStartupTrace } from "./startup-trace.cjs";
import type { SessionEvent } from "./types.cjs";

import { createWindowOptions } from "./window-options.cjs";

type BackendStatus = {
  state: string;
  message?: string;
  sessionId?: string;
  backendUrl?: string | null;
};

const PROJECT_ROOT = path.resolve(__dirname, "../../..");
const smokeMode = process.argv.includes("--smoke");
const trace = createStartupTrace();
trace.mark("electron.main.loaded", { smoke: smokeMode });

let backendProcess: ChildProcess | null = null;
let backendUrl: string | null = null;
let sessionId: string | null = null;
let mainWindow: BrowserWindow | null = null;
let firstSessionEventSeen = false;
const pendingProjectSessions = new Map<string, Promise<string | null>>();
// Every session the app has opened keeps its own live SSE subscription so a
// background run keeps streaming while the user works in another session. Keyed
// by session id; closed only on quit.
const subscriptions = new Map<string, Subscription>();

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
// transport. The renderer names the target session so a message always lands in
// the conversation it was typed into, even if several are running at once.
ipcMain.handle("nav:send-message", async (_event, request) => {
  const { sessionId: targetSessionId, text } = request ?? {};
  if (!backendUrl || !targetSessionId || !text) {
    throw new Error("chat session is not ready");
  }
  await sendRpc({
    backendUrl,
    method: "session.sendMessage",
    params: { sessionId: targetSessionId, text },
  });
});

ipcMain.handle("nav:stop", async (_event, requestedSessionId) => {
  if (!backendUrl || !requestedSessionId) {
    throw new Error("chat session is not ready");
  }
  const response = await sendRpc({
    backendUrl,
    method: "session.stop",
    params: { sessionId: requestedSessionId },
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

// The composer's "Start in" preference is persisted in Main (not the renderer)
// because startup runs `openSession` before the renderer exists — the file is
// the only place the preselected mode can live so launch honors it.
ipcMain.handle("nav:get-session-mode", () =>
  readSessionMode(sessionModeFilePath()),
);

ipcMain.handle("nav:set-session-mode", (_event, mode) =>
  writeSessionMode(sessionModeFilePath(), mode),
);

function createMainWindow(): BrowserWindow {
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
  window.webContents.setWindowOpenHandler(({ url }) => {
    openInExternalBrowser(url);
    return { action: "deny" };
  });
  window.webContents.on("will-navigate", (event, url) => {
    if (openInExternalBrowser(url)) {
      event.preventDefault();
    }
  });
  trace.mark("electron.window.load_file.start");
  window
    .loadFile(path.join(__dirname, "..", "renderer", "dist", "index.html"))
    .then(() => {
      trace.mark("electron.window.load_file.end");
    })
    .catch((error: Error) => {
      trace.mark("electron.window.load_file.failed", { error: error.message });
    });
  return window;
}

function openInExternalBrowser(url: string): boolean {
  if (!isExternalBrowserUrl(url)) {
    return false;
  }
  shell.openExternal(url).catch((error: Error) => {
    trace.mark("electron.external_link.failed", {
      url,
      error: error.message,
    });
  });
  return true;
}

async function startChatSession(window: BrowserWindow | null): Promise<void> {
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
    trace.mark("electron.backend.ready", { pid: backend.child.pid });

    activateSession(window, await openSession(), { startup: true });

    if (smokeMode) {
      await runSmokeTurn();
    }
  } catch (error) {
    const message = (error as Error).message;
    trace.mark("electron.startup.failed", { error: message });
    sendStatus(window, { state: "backend-error", message });
    if (smokeMode) {
      console.error(error);
      printStartupSummary();
      app.exit(1);
    }
  }
}

// Bring `id` to the foreground: make sure Main is streaming its events and note
// it as the primary session (used by smoke mode and as an RPC default). Prior
// sessions keep their own subscriptions open, so their runs keep streaming in
// the background. The renderer routes every event by its session id.
function activateSession(
  window: BrowserWindow | null,
  id: string,
  { startup = false }: { startup?: boolean } = {},
): void {
  sessionId = id;
  ensureSubscription(window, id, { startup, announce: startup });
}

// Subscribe to a session's event feed once and keep it open. Re-activating an
// already-subscribed session is a no-op (its backlog already streamed and lives
// in the renderer), so switching back never replays or duplicates a transcript.
// `announce` sends the renderer the single startup `connected` status; later
// sessions are activated by the renderer itself, which must not be told to jump
// its active conversation.
function ensureSubscription(
  window: BrowserWindow | null,
  id: string,
  {
    startup = false,
    announce = false,
  }: { startup?: boolean; announce?: boolean } = {},
): void {
  if (subscriptions.has(id)) {
    if (announce) {
      sendConnected(window, id);
    }
    return;
  }
  if (!backendUrl) {
    return;
  }
  markStartup(startup, "electron.sse.subscribe.start", { session_id: id });
  const subscription = subscribeToSessionEvents({
    backendUrl,
    sessionId: id,
    onOpen({ statusCode }) {
      markStartup(startup, "electron.sse.open", {
        session_id: id,
        status_code: statusCode,
      });
      if (statusCode !== 200) {
        return;
      }
      if (announce) {
        sendConnected(window, id);
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
      // A stream can fire onError more than once (request and response paths),
      // possibly after the session was re-subscribed. Only tear down the entry
      // if it is still this exact subscription, never a newer replacement.
      if (subscriptions.get(id) !== subscription) {
        return;
      }
      subscriptions.delete(id);
      // Name the session whose stream died so the renderer reports the error in
      // that conversation, not whichever one happens to be on screen.
      sendStatus(window, {
        state: "stream-error",
        message: error.message,
        sessionId: id,
      });
      if (smokeMode) {
        console.error(error);
        printStartupSummary();
        app.exit(1);
      }
    },
  });
  subscriptions.set(id, subscription);
}

// Smoke mode drives the turn from Main and quits on `run.completed`; telling the
// renderer it is connected can start sidebar/model refreshes that race the
// intentional shutdown, so it is suppressed there.
function sendConnected(window: BrowserWindow | null, id: string): void {
  if (smokeMode) {
    return;
  }
  sendStatus(window, { state: "connected", backendUrl, sessionId: id });
}

// Reopen the most recent conversation so sessions persist across launches,
// honoring the persisted "Start in" preference: a worktree launch resumes (or
// creates) a worktree session and never the main checkout. Smoke mode ignores
// the preference and starts fresh in local mode so the offline run stays
// deterministic regardless of machine-local state.
async function openSession(): Promise<string> {
  trace.mark("electron.session.open.start");
  const sessionMode = smokeMode
    ? "local"
    : readSessionMode(sessionModeFilePath());
  if (!smokeMode) {
    const resumedSessionId = await resumeStartupSession(sessionMode);
    if (resumedSessionId) {
      trace.mark("electron.session.open.end", {
        mode: "resumed",
        session_mode: sessionMode,
      });
      return resumedSessionId;
    }
  }
  const created = await tracedRpc("session.create", {
    cwd: PROJECT_ROOT,
    mode: sessionMode,
  });
  trace.mark("electron.session.open.end", {
    mode: "created",
    session_mode: sessionMode,
  });
  return created.result.sessionId;
}

// Resume the newest session for the requested mode, or null when none exists or
// resumption fails so the caller creates a fresh one instead of leaving the app
// stuck on launch.
async function resumeStartupSession(
  sessionMode: SessionMode,
): Promise<string | null> {
  const startupSessionId = await latestSessionIdForMode(sessionMode);
  if (!startupSessionId) {
    return null;
  }
  // Resumption can fail on a pruned session, corrupt DB, or schema mismatch.
  try {
    const resumed = await tracedRpc("session.resume", {
      sessionId: startupSessionId,
    });
    return resumed.result.sessionId;
  } catch (error) {
    const message = (error as Error).message;
    trace.mark("electron.session.resume.fallback", { error: message });
    console.error(
      `failed to resume session ${startupSessionId}, starting fresh: ${message}`,
    );
    return null;
  }
}

// The newest resumable session id for the mode, or null when none matches.
// Worktree mode matches only worktree sessions for this checkout (via the same
// helper Add Project uses), so a worktree launch is never silently downgraded
// to the main checkout.
async function latestSessionIdForMode(
  sessionMode: SessionMode,
): Promise<string | null> {
  if (sessionMode === "worktree") {
    return findExistingProjectSession(PROJECT_ROOT, "worktree");
  }
  const latest = await tracedRpc("session.latest", { cwd: PROJECT_ROOT });
  return latest.result?.sessionId ?? null;
}

function sessionModeFilePath(): string {
  return path.join(app.getPath("userData"), "nav-ui.json");
}

async function createBackendSession(
  cwd: string | null,
  mode: SessionMode,
): Promise<string> {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  const params = cwd ? { cwd, mode } : { mode };
  const created = await sendRpc({
    backendUrl,
    method: "session.create",
    params,
  });
  return created.result.sessionId;
}

async function openOrCreateProjectSession(
  workspaceRoot: string,
  mode: SessionMode = "local",
): Promise<string | null> {
  // The resolved session depends on both the root and the mode, so dedupe
  // in-flight lookups per (mode, root) — keying on the root alone would let a
  // concurrent local and worktree open of the same path share one promise and
  // return a session of the wrong mode.
  const pendingKey = `${mode} ${workspaceRoot}`;
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

async function findExistingProjectSession(
  workspaceRoot: string,
  mode: SessionMode,
): Promise<string | null> {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
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

async function createProjectSession(
  workspaceRoot: string,
  mode: SessionMode,
): Promise<string> {
  return createBackendSession(workspaceRoot, mode);
}

function normalizeNewSessionRequest(request: unknown): {
  cwd: string | null;
  mode: SessionMode | null;
} {
  if (!request || typeof request !== "object") {
    return { cwd: null, mode: null };
  }
  const { cwd, mode } = request as { cwd?: unknown; mode?: unknown };
  return {
    cwd: normalizeOptionalCwd(cwd),
    mode: normalizeOptionalMode(mode),
  };
}

function normalizeOptionalCwd(cwd: unknown): string | null {
  if (typeof cwd !== "string") {
    return null;
  }
  const trimmed = cwd.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function normalizeOptionalMode(mode: unknown): SessionMode | null {
  return mode === "local" || mode === "worktree" ? mode : null;
}

function forwardEvent(window: BrowserWindow | null, event: SessionEvent): void {
  if (window && !window.isDestroyed()) {
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

async function runSmokeTurn(): Promise<void> {
  trace.mark("electron.smoke.turn.start");
  await tracedRpc("session.sendMessage", {
    sessionId,
    text: "smoke test message",
  });
  trace.mark("electron.smoke.turn.accepted");
}

function sendStatus(window: BrowserWindow | null, status: BackendStatus): void {
  if (window && !window.isDestroyed()) {
    window.webContents.send("nav:backend-status", status);
  }
}

function stopBackend(): void {
  for (const subscription of subscriptions.values()) {
    subscription.close();
  }
  subscriptions.clear();
  backendProcess?.kill();
  backendProcess = null;
}

async function tracedRpc(method: string, params?: unknown) {
  if (!backendUrl) {
    throw new Error("chat session is not ready");
  }
  trace.mark(`electron.rpc.${method}.start`);
  try {
    const response = await sendRpc({ backendUrl, method, params });
    trace.mark(`electron.rpc.${method}.end`);
    return response;
  } catch (error) {
    trace.mark(`electron.rpc.${method}.failed`, {
      error: (error as Error).message,
    });
    throw error;
  }
}

function printStartupSummary(): void {
  const summary = trace.summaryLine();
  if (summary) {
    console.log(summary);
  }
}

function markStartup(
  startup: boolean,
  event: string,
  fields?: Record<string, unknown>,
): void {
  if (startup) {
    trace.mark(event, fields);
  }
}

function markFirstStartupEvent(id: string, event: SessionEvent): void {
  if (firstSessionEventSeen) {
    return;
  }
  firstSessionEventSeen = true;
  trace.mark("electron.sse.first_event", {
    session_id: id,
    event_type: event.type,
  });
}
