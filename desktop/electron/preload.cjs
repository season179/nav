const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("nav", {
  onBackendStatus(callback) {
    return subscribe("nav:backend-status", callback);
  },
  onSessionEvent(callback) {
    return subscribe("nav:session-event", callback);
  },
});

function subscribe(channel, callback) {
  if (typeof callback !== "function") {
    throw new TypeError(`${channel} listener must be a function`);
  }

  const listener = (_event, payload) => {
    callback(payload);
  };

  ipcRenderer.on(channel, listener);
  return () => {
    ipcRenderer.removeListener(channel, listener);
  };
}
