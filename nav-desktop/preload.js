const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("navApp", {
  version: "0.1.0",
  getWorkspace: () => ipcRenderer.invoke("workspace:get"),
  selectWorkspace: () => ipcRenderer.invoke("workspace:select"),
  runAgent: (prompt) => ipcRenderer.invoke("agent:run", { prompt }),
  onAgentEvent: (callback) => {
    if (typeof callback !== "function") {
      return () => {};
    }

    const listener = (_event, payload) => callback(payload);
    ipcRenderer.on("agent:event", listener);
    return () => ipcRenderer.removeListener("agent:event", listener);
  },
});
