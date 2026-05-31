const { app, BrowserWindow } = require("electron");
const path = require("node:path");
const { subscribeToSessionEvents } = require("./backend-client.cjs");
const { startLocalBackend } = require("./backend-process.cjs");
const { createWindowOptions } = require("./window-options.cjs");

const FIXTURE_SESSION_ID = "019f2f6f-f178-7a72-9f28-000000000100";
const PROJECT_ROOT = path.resolve(__dirname, "../..");

let backendProcess = null;
let eventSubscription = null;
const smokeMode = process.argv.includes("--smoke");

app.whenReady().then(async () => {
  const window = createMainWindow();
  await startReadOnlyStream(window);
});

app.on("before-quit", () => {
  stopBackend();
});

app.on("window-all-closed", () => {
  stopBackend();
  app.quit();
});

function createMainWindow() {
  const window = new BrowserWindow(
    createWindowOptions({
      preloadPath: path.join(__dirname, "preload.cjs"),
    }),
  );

  window.loadFile(path.join(__dirname, "renderer", "index.html"));
  return window;
}

async function startReadOnlyStream(window) {
  sendStatus(window, { state: "starting-backend" });

  try {
    const backend = await startLocalBackend({ projectRoot: PROJECT_ROOT });
    backendProcess = backend.child;
    sendStatus(window, {
      state: "connected",
      backendUrl: backend.url,
      sessionId: FIXTURE_SESSION_ID,
    });

    eventSubscription = subscribeToSessionEvents({
      backendUrl: backend.url,
      sessionId: FIXTURE_SESSION_ID,
      onEvent(event) {
        window.webContents.send("nav:session-event", event);
        if (smokeMode && event.type === "run.completed") {
          console.log("nav electron smoke received run.completed");
          app.quit();
        }
      },
      onError(error) {
        sendStatus(window, { state: "stream-error", message: error.message });
        if (smokeMode) {
          console.error(error);
          app.exit(1);
        }
      },
    });
  } catch (error) {
    sendStatus(window, { state: "backend-error", message: error.message });
    if (smokeMode) {
      console.error(error);
      app.exit(1);
    }
  }
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
