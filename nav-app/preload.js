const { contextBridge } = require("electron");

contextBridge.exposeInMainWorld("navApp", {
  version: "0.1.0",
});
