const { app, BrowserWindow, ipcMain } = require("electron");
const path = require("node:path");
const { subscribeToSessionEvents, sendRpc } = require("./backend-client.cjs");
const { startLocalBackend } = require("./backend-process.cjs");
const { createWindowOptions } = require("./window-options.cjs");

const PROJECT_ROOT = path.resolve(__dirname, "../..");
const smokeMode = process.argv.includes("--smoke");

let backendProcess = null;
let eventSubscription = null;
let backendUrl = null;
let sessionId = null;

app.whenReady().then(async () => {
  const window = createMainWindow();
  await startChatSession(window);
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

function createMainWindow() {
  const window = new BrowserWindow(
    createWindowOptions({ preloadPath: path.join(__dirname, "preload.cjs") }),
  );
  window.loadFile(path.join(__dirname, "renderer", "index.html"));
  return window;
}

async function startChatSession(window) {
  sendStatus(window, { state: "starting-backend" });

  try {
    // Tests and offline smoke use the deterministic mock; a real launch
    // inherits the user's environment (NAV_API_KEY, etc.).
    const backend = await startLocalBackend({
      projectRoot: PROJECT_ROOT,
      env: smokeMode ? { NAV_MOCK_MODEL: "1" } : {},
    });
    backendProcess = backend.child;
    backendUrl = backend.url;

    sessionId = await openSession();

    sendStatus(window, { state: "connected", backendUrl, sessionId });

    eventSubscription = subscribeToSessionEvents({
      backendUrl,
      sessionId,
      onEvent(event) {
        forwardEvent(window, event);
      },
      onError(error) {
        sendStatus(window, { state: "stream-error", message: error.message });
        if (smokeMode) {
          console.error(error);
          app.exit(1);
        }
      },
    });

    if (smokeMode) {
      await runSmokeTurn();
    }
  } catch (error) {
    sendStatus(window, { state: "backend-error", message: error.message });
    if (smokeMode) {
      console.error(error);
      app.exit(1);
    }
  }
}

// Reopen the most recent conversation so sessions persist across launches.
// Smoke mode always starts fresh to stay deterministic.
async function openSession() {
  if (!smokeMode) {
    const latest = await sendRpc({ backendUrl, method: "session.latest" });
    if (latest.result?.sessionId) {
      // If resumption fails (pruned session, corrupt DB, schema mismatch), fall
      // back to a fresh session rather than leaving the app stuck on launch.
      try {
        const resumed = await sendRpc({
          backendUrl,
          method: "session.resume",
          params: { sessionId: latest.result.sessionId },
        });
        return resumed.result.sessionId;
      } catch (error) {
        console.error(
          `failed to resume session ${latest.result.sessionId}, starting fresh: ${error.message}`,
        );
      }
    }
  }
  const created = await sendRpc({ backendUrl, method: "session.create" });
  return created.result.sessionId;
}

function forwardEvent(window, event) {
  if (!window.isDestroyed()) {
    window.webContents.send("nav:session-event", event);
  }
  if (smokeMode && event.type === "run.completed") {
    console.log("nav electron smoke received run.completed");
    app.quit();
  }
  if (smokeMode && event.type === "run.failed") {
    console.error(`nav electron smoke run failed: ${event.error}`);
    app.exit(1);
  }
}

async function runSmokeTurn() {
  await sendRpc({
    backendUrl,
    method: "session.sendMessage",
    params: { sessionId, text: "smoke test message" },
  });
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
