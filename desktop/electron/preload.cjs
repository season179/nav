const { contextBridge, ipcRenderer } = require("electron");

// This preload runs sandboxed (`sandbox: true`), where `require` is limited to
// `electron` and a few builtins — it cannot load relative project files. So the
// boundary validation is inlined here rather than imported from
// `request-validation.cjs`. That standalone module is kept in lockstep and is
// what the unit tests exercise; see tests/electron_request_validation.test.cjs.
function normalizeMessageText(value) {
  if (typeof value !== "string") {
    throw new TypeError("message text must be a string");
  }
  const text = value.trim();
  if (text.length === 0) {
    throw new Error("message text must not be empty");
  }
  return text;
}

function normalizeSessionId(value) {
  if (typeof value !== "string") {
    throw new TypeError("session id must be a string");
  }
  const id = value.trim();
  if (id.length === 0) {
    throw new Error("session id must not be empty");
  }
  return id;
}

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
  // List persisted sessions for the sidebar.
  listSessions() {
    return ipcRenderer.invoke("nav:list-sessions");
  },
  // Switch the active conversation to an existing session.
  switchSession(sessionId) {
    return ipcRenderer.invoke(
      "nav:switch-session",
      normalizeSessionId(sessionId),
    );
  },
  // Start a fresh conversation and make it active.
  newSession() {
    return ipcRenderer.invoke("nav:new-session");
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
