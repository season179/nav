const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("navApp", {
  version: "0.1.0",
  getWorkspace: () => ipcRenderer.invoke("workspace:get"),
  selectWorkspace: () => ipcRenderer.invoke("workspace:select"),
});
