import { contextBridge, ipcRenderer } from "electron";

import type {
  FlueConnection,
  FlueServerStatus,
} from "../lib/flue-connection.js";

contextBridge.exposeInMainWorld("navDesktop", {
  getFlueConnection: async (): Promise<FlueConnection> =>
    await ipcRenderer.invoke("flue:getConnection"),
  onFlueStatus: (callback: (status: FlueServerStatus) => void) => {
    const handler = (
      _event: Electron.IpcRendererEvent,
      status: FlueServerStatus,
    ) => {
      callback(status);
    };

    ipcRenderer.on("flue:status", handler);

    return () => {
      ipcRenderer.removeListener("flue:status", handler);
    };
  },
});
