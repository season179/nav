const { contextBridge, ipcRenderer } = require("electron");
const { normalizeMessageText } = require("./request-validation.cjs");

contextBridge.exposeInMainWorld("nav", {
  onBackendStatus(callback) {
    return subscribe("nav:backend-status", callback);
  },
  onSessionEvent(callback) {
    return subscribe("nav:session-event", callback);
  },
  // Send one chat message. The text is validated here so the renderer can only
  // ever hand Main a clean string — never an arbitrary IPC payload.
  sessionSendMessage(text) {
    return ipcRenderer.invoke("nav:send-message", normalizeMessageText(text));
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
