const { app, BrowserWindow, dialog, ipcMain } = require("electron");
const fs = require("node:fs/promises");
const path = require("node:path");

const workspaceStatePath = () =>
  path.join(app.getPath("userData"), "workspace.json");

if (process.env.NAV_USER_DATA_DIR) {
  app.setPath("userData", process.env.NAV_USER_DATA_DIR);
}

const toWorkspace = (directoryPath) => ({
  path: directoryPath,
  name: path.basename(directoryPath) || directoryPath,
});

const isDirectory = async (directoryPath) => {
  try {
    const stat = await fs.stat(directoryPath);
    return stat.isDirectory();
  } catch {
    return false;
  }
};

const readWorkspace = async () => {
  try {
    const raw = await fs.readFile(workspaceStatePath(), "utf8");
    const stored = JSON.parse(raw);

    if (typeof stored?.path !== "string") {
      return null;
    }

    if (!(await isDirectory(stored.path))) {
      return null;
    }

    return toWorkspace(stored.path);
  } catch {
    return null;
  }
};

const saveWorkspace = async (directoryPath) => {
  await fs.mkdir(path.dirname(workspaceStatePath()), { recursive: true });
  await fs.writeFile(
    workspaceStatePath(),
    JSON.stringify({ path: directoryPath }, null, 2),
    "utf8",
  );
};

const createWindow = () => {
  const mainWindow = new BrowserWindow({
    width: 1120,
    height: 760,
    minWidth: 900,
    minHeight: 620,
    title: "nav",
    titleBarStyle: "hiddenInset",
    backgroundColor: "#202020",
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });

  mainWindow.loadFile(path.join(__dirname, "../ui/index.html"));
};

app.whenReady().then(() => {
  ipcMain.handle("workspace:get", readWorkspace);

  ipcMain.handle("workspace:select", async (event) => {
    const window = BrowserWindow.fromWebContents(event.sender);
    const options = {
      title: "Select working directory",
      properties: ["openDirectory"],
    };
    const result = window
      ? await dialog.showOpenDialog(window, options)
      : await dialog.showOpenDialog(options);

    if (result.canceled || result.filePaths.length === 0) {
      return null;
    }

    const directoryPath = result.filePaths[0];
    if (!(await isDirectory(directoryPath))) {
      return null;
    }

    await saveWorkspace(directoryPath);
    return toWorkspace(directoryPath);
  });

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
