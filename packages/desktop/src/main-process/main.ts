import path from "node:path";
import { fileURLToPath } from "node:url";
import { app, BrowserWindow } from "electron";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const appBackgroundColor = "#090b0c";

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
    },
    width: 1024,
  });

  const devServerUrl = process.env.VITE_DEV_SERVER_URL;

  if (devServerUrl) {
    void window.loadURL(devServerUrl);
    return;
  }

  void window.loadFile(path.join(__dirname, "../../dist/index.html"));
};

void app.whenReady().then(() => {
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
