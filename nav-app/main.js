const { app, BrowserWindow, dialog, ipcMain } = require("electron");
const { spawn } = require("node:child_process");
const fs = require("node:fs/promises");
const path = require("node:path");

const APP_NAME = "nav-app";
const APP_ROOT = path.resolve(__dirname, "..");
const CARGO_COMMAND = process.platform === "win32" ? "cargo.exe" : "cargo";

app.setName(APP_NAME);

const activeAgents = new Map();

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

const sendAgentEvent = (webContents, payload) => {
  if (!webContents.isDestroyed()) {
    webContents.send("agent:event", payload);
  }
};

const runAgent = async (event, input) => {
  const prompt = typeof input?.prompt === "string" ? input.prompt.trim() : "";
  if (!prompt) {
    throw new Error("Prompt is required.");
  }

  const webContents = event.sender;
  const webContentsId = webContents.id;
  if (activeAgents.has(webContentsId)) {
    throw new Error("Nav is already running in this window.");
  }

  const workspace = await readWorkspace();
  if (!workspace) {
    throw new Error("Select a working directory first.");
  }

  const manifestPath = path.join(APP_ROOT, "Cargo.toml");
  const child = spawn(
    CARGO_COMMAND,
    ["run", "--quiet", "--manifest-path", manifestPath, "--", prompt],
    {
      cwd: workspace.path,
      env: process.env,
      windowsHide: true,
    },
  );

  activeAgents.set(webContentsId, child);
  sendAgentEvent(webContents, { type: "started", workspace });

  return await new Promise((resolve) => {
    let settled = false;
    let stdout = "";
    let stderr = "";

    const settle = (result) => {
      if (settled) {
        return;
      }
      settled = true;
      activeAgents.delete(webContentsId);
      resolve(result);
    };

    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");

    child.stdout.on("data", (chunk) => {
      stdout += chunk;
      sendAgentEvent(webContents, { type: "stdout", text: chunk });
    });

    child.stderr.on("data", (chunk) => {
      stderr += chunk;
      sendAgentEvent(webContents, { type: "stderr", text: chunk });
    });

    child.on("error", (error) => {
      sendAgentEvent(webContents, {
        type: "error",
        message: error.message,
      });
      settle({ ok: false, error: error.message, stdout, stderr });
    });

    child.on("close", (exitCode, signal) => {
      const ok = exitCode === 0;
      sendAgentEvent(webContents, {
        type: "done",
        ok,
        exitCode,
        signal,
      });
      settle({ ok, exitCode, signal, stdout, stderr });
    });
  });
};

const createWindow = () => {
  const mainWindow = new BrowserWindow({
    width: 1120,
    height: 760,
    minWidth: 900,
    minHeight: 620,
    title: APP_NAME,
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
  ipcMain.handle("agent:run", runAgent);

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

app.on("before-quit", () => {
  for (const child of activeAgents.values()) {
    child.kill();
  }
  activeAgents.clear();
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") {
    app.quit();
  }
});
