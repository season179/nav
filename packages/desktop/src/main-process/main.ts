import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  app,
  BrowserWindow,
  dialog,
  ipcMain,
  type OpenDialogOptions,
  type WebFrameMain,
} from "electron";

import { FlueServer } from "./flue-server.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appBackgroundColor = "#090b0c";
const devServerUrl = process.env.VITE_DEV_SERVER_URL;
const trustedDevOrigin = devServerUrl ? new URL(devServerUrl).origin : null;

const flueServer = new FlueServer({
  devServerUrl,
  mainProcessDir: __dirname,
  onStatusChange: (status) => {
    for (const window of BrowserWindow.getAllWindows()) {
      window.webContents.send("flue:status", status);
    }
  },
});

const isTrustedSender = (frame: WebFrameMain) => {
  try {
    const url = new URL(frame.url);

    if (url.protocol === "file:") {
      return true;
    }

    return trustedDevOrigin !== null && url.origin === trustedDevOrigin;
  } catch {
    return false;
  }
};

const createWindow = () => {
  const window = new BrowserWindow({
    backgroundColor: appBackgroundColor,
    height: 768,
    titleBarOverlay: {
      color: appBackgroundColor,
      symbolColor: "#f9fbfa",
    },
    titleBarStyle: process.platform === "darwin" ? "hiddenInset" : "hidden",
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      preload: path.join(__dirname, "preload.cjs"),
    },
    width: 1024,
  });

  window.webContents.once("did-finish-load", () => {
    window.webContents.send("flue:status", flueServer.getStatus());
  });

  if (devServerUrl) {
    void window.loadURL(devServerUrl);
    return;
  }

  void window.loadFile(path.join(__dirname, "../../dist/index.html"));
};

void app.whenReady().then(() => {
  ipcMain.handle("flue:getConnection", async (event) => {
    if (!event.senderFrame || !isTrustedSender(event.senderFrame)) {
      throw new Error("Untrusted renderer requested Flue connection details.");
    }

    return await flueServer.getConnection();
  });

  ipcMain.handle("dialog:pickProjectDirectory", async (event) => {
    if (!event.senderFrame || !isTrustedSender(event.senderFrame)) {
      throw new Error("Untrusted renderer requested a project directory.");
    }

    const owner = BrowserWindow.fromWebContents(event.sender);
    const options: OpenDialogOptions = { properties: ["openDirectory"] };
    const result = owner
      ? await dialog.showOpenDialog(owner, options)
      : await dialog.showOpenDialog(options);

    return result.canceled ? null : (result.filePaths[0] ?? null);
  });

  void flueServer.start().catch(() => undefined);

  createWindow();

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) {
      createWindow();
    }
  });
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});

app.on("before-quit", () => {
  flueServer.stop();
});
