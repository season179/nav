import { contextBridge, ipcRenderer } from "electron";

const THINKING_LEVELS = new Set([
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
]);
const THINKING_LEVEL_ERROR =
  "thinking level must be off, minimal, low, medium, high, or xhigh";

// This preload runs sandboxed (`sandbox: true`), where `require` is limited to
// `electron` and a few builtins — it cannot load relative project files. So the
// boundary validation is inlined here rather than imported from
// `request-validation.cts`. That standalone module is kept in lockstep and is
// what the unit tests exercise; see tests/electron_request_validation.test.cts.
function normalizeMessageText(value: unknown): string {
  return normalizeRequiredString(value, "message text");
}

function normalizeSessionId(value: unknown): string {
  return normalizeRequiredString(value, "session id");
}

function normalizeModelProvider(value: unknown): string {
  return normalizeRequiredString(value, "model provider");
}

function normalizeModelId(value: unknown): string {
  return normalizeRequiredString(value, "model id");
}

function normalizeThinkingLevel(value: unknown): string {
  const level = normalizeRequiredString(value, "thinking level");
  if (!THINKING_LEVELS.has(level)) {
    throw new Error(THINKING_LEVEL_ERROR);
  }
  return level;
}

function normalizeOptionalThinkingLevel(value: unknown): string | null {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeThinkingLevel(value);
}

function normalizeOptionalWorkspaceRoot(value: unknown): string | null {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeRequiredString(value, "workspace root");
}

function normalizeOptionalSessionMode(value: unknown): string | null {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeSessionMode(value);
}

function normalizeSessionMode(value: unknown): string {
  const mode = normalizeRequiredString(value, "session mode");
  if (mode !== "local" && mode !== "worktree") {
    throw new Error("session mode must be local or worktree");
  }
  return mode;
}

function normalizeRequiredString(value: unknown, label: string): string {
  if (typeof value !== "string") {
    throw new TypeError(`${label} must be a string`);
  }
  const text = value.trim();
  if (text.length === 0) {
    throw new Error(`${label} must not be empty`);
  }
  return text;
}

contextBridge.exposeInMainWorld("nav", {
  onBackendStatus(callback: (status: unknown) => void) {
    return subscribe("nav:backend-status", callback);
  },
  onSessionEvent(callback: (event: unknown) => void) {
    return subscribe("nav:session-event", callback);
  },
  // Send one chat message to a specific session. Both the target session and
  // the text are validated here so the renderer can only ever hand Main a clean
  // payload — never an arbitrary IPC value.
  sessionSendMessage(sessionId: unknown, text: unknown) {
    return ipcRenderer.invoke("nav:send-message", {
      sessionId: normalizeSessionId(sessionId),
      text: normalizeMessageText(text),
    });
  },
  // Stop a specific session's in-flight run.
  sessionStop(sessionId: unknown) {
    return ipcRenderer.invoke("nav:stop", normalizeSessionId(sessionId));
  },
  // List persisted sessions for the sidebar.
  listSessions() {
    return ipcRenderer.invoke("nav:list-sessions");
  },
  // Pick a directory and create a fresh session inside it.
  createProject(mode: unknown) {
    return ipcRenderer.invoke("nav:create-project", {
      mode: normalizeOptionalSessionMode(mode),
    });
  },
  // The active model's display info, shown beneath the composer.
  modelInfo(sessionId: unknown) {
    return ipcRenderer.invoke(
      "nav:model-info",
      sessionId == null ? undefined : normalizeSessionId(sessionId),
    );
  },
  // Configured models safe to show in the model picker.
  modelList() {
    return ipcRenderer.invoke("nav:model-list");
  },
  // Switch the active backend model to a configured provider/model pair.
  switchModel(provider: unknown, model: unknown, thinkingLevel: unknown) {
    const request: {
      provider: string;
      model: string;
      thinkingLevel?: string;
    } = {
      provider: normalizeModelProvider(provider),
      model: normalizeModelId(model),
    };
    const normalizedThinking = normalizeOptionalThinkingLevel(thinkingLevel);
    if (normalizedThinking) {
      request.thinkingLevel = normalizedThinking;
    }
    return ipcRenderer.invoke("nav:switch-model", request);
  },
  // Switch only the active model's thinking level.
  switchThinking(thinkingLevel: unknown) {
    return ipcRenderer.invoke(
      "nav:switch-thinking",
      normalizeThinkingLevel(thinkingLevel),
    );
  },
  // Debug context stacks captured for a session's model calls.
  sessionStacks(sessionId: unknown) {
    return ipcRenderer.invoke(
      "nav:session-stacks",
      sessionId == null ? undefined : normalizeSessionId(sessionId),
    );
  },
  // Advisory scan for whether stack records are currently retained.
  sessionStackAvailability(sessionId: unknown) {
    return ipcRenderer.invoke(
      "nav:session-stack-availability",
      sessionId == null ? undefined : normalizeSessionId(sessionId),
    );
  },
  // Switch the active conversation to an existing session.
  switchSession(sessionId: unknown) {
    return ipcRenderer.invoke(
      "nav:switch-session",
      normalizeSessionId(sessionId),
    );
  },
  // Start a fresh conversation and make it active.
  newSession(workspaceRoot: unknown, mode: unknown) {
    return ipcRenderer.invoke("nav:new-session", {
      cwd: normalizeOptionalWorkspaceRoot(workspaceRoot),
      mode: normalizeOptionalSessionMode(mode),
    });
  },
  // Read the persisted "Start in" preference so the composer can initialize it.
  getSessionMode() {
    return ipcRenderer.invoke("nav:get-session-mode");
  },
  // Persist the "Start in" preference so the next launch starts in this mode.
  setSessionMode(mode: unknown) {
    return ipcRenderer.invoke(
      "nav:set-session-mode",
      normalizeSessionMode(mode),
    );
  },
});

function subscribe(channel: string, callback: unknown): () => void {
  if (typeof callback !== "function") {
    throw new TypeError(`${channel} listener must be a function`);
  }

  const listener = (_event: unknown, payload: unknown) => {
    (callback as (payload: unknown) => void)(payload);
  };

  ipcRenderer.on(channel, listener);
  return () => {
    ipcRenderer.removeListener(channel, listener);
  };
}
