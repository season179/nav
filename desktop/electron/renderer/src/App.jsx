import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import Composer from "./components/Composer.jsx";
import Sidebar from "./components/Sidebar.jsx";
import StacksPage from "./components/StacksPage.jsx";
import Transcript from "./components/Transcript.jsx";
import {
  appendMessage,
  createSessionState,
  reduceSessions,
} from "./lib/session-runtime.mjs";
import {
  STACK_AVAILABILITY_RECHECK_DELAY_MS,
  shouldRefreshStackAvailabilityForEvent,
} from "./lib/stack-availability.mjs";

// Shown when a session has no state yet (e.g. before its first event arrives),
// so the composer and transcript always have a stable object to read.
const EMPTY_SESSION_STATE = createSessionState();
// Where startup/connection errors land before any real session exists.
const SYSTEM_SESSION_ID = "system";

export default function App() {
  const [connected, setConnected] = useState(false);
  const [activeSessionId, setActiveSessionId] = useState(null);
  const [sessionSummaries, setSessionSummaries] = useState([]);
  // Every live session's transcript and run state, keyed by id. Routing events
  // here by session id is what keeps concurrent sessions from interfering.
  const [sessionStates, setSessionStates] = useState({});
  const [attentionSessionIds, setAttentionSessionIds] = useState(
    () => new Set(),
  );
  const [modelOptions, setModelOptions] = useState([]);
  const [modelSwitching, setModelSwitching] = useState(false);
  const [activeView, setActiveView] = useState("chat");
  const [newSessionMode, setNewSessionMode] = useState("local");

  const connectedRef = useRef(false);
  const activeSessionIdRef = useRef(null);
  // Imperative per-session run bookkeeping read synchronously by the stop/send
  // paths (React state would be stale inside the async stop loop). `running`
  // mirrors the view state's flag; the rest track one in-flight run.
  const runtimesRef = useRef(new Map());
  const sessionModeTouchedRef = useRef(false);

  const setConnectedState = useCallback((isConnected) => {
    connectedRef.current = isConnected;
    setConnected(isConnected);
  }, []);

  const setActiveSession = useCallback((sessionId) => {
    activeSessionIdRef.current = sessionId;
    setActiveSessionId(sessionId);
    if (sessionId) {
      setAttentionSessionIds((current) => {
        if (!current.has(sessionId)) {
          return current;
        }
        const next = new Set(current);
        next.delete(sessionId);
        return next;
      });
    }
  }, []);

  const runtimeFor = useCallback((sessionId) => {
    let runtime = runtimesRef.current.get(sessionId);
    if (!runtime) {
      runtime = {
        running: false,
        runStarted: false,
        stopRequested: false,
        stopRpcInFlight: false,
        stopSentForActiveRun: false,
        stackRequest: 0,
        modelInfoRequest: 0,
        // Event ids already applied, so a backlog replay (e.g. after a
        // stream-error re-subscribes) is ignored instead of duplicating the
        // transcript.
        seenEventIds: new Set(),
      };
      runtimesRef.current.set(sessionId, runtime);
    }
    return runtime;
  }, []);

  // Update one session's view state, creating it on first touch so events for a
  // brand-new (or backgrounded) session never need pre-registration.
  const updateSessionState = useCallback((sessionId, updater) => {
    if (!sessionId) {
      return;
    }
    setSessionStates((current) => {
      const previous = current[sessionId] ?? createSessionState();
      const next = updater(previous);
      if (next === previous) {
        return current;
      }
      return { ...current, [sessionId]: next };
    });
  }, []);

  const appendSessionMessage = useCallback(
    (sessionId, role, text) => {
      updateSessionState(sessionId, (state) =>
        appendMessage(state, role, text),
      );
    },
    [updateSessionState],
  );

  const setSessionRunning = useCallback(
    (sessionId, isRunning) => {
      runtimeFor(sessionId).running = isRunning;
      updateSessionState(sessionId, (state) =>
        state.running === isRunning ? state : { ...state, running: isRunning },
      );
    },
    [runtimeFor, updateSessionState],
  );

  const setSessionStopPending = useCallback(
    (sessionId, isPending) => {
      updateSessionState(sessionId, (state) =>
        state.stopPending === isPending
          ? state
          : { ...state, stopPending: isPending },
      );
    },
    [updateSessionState],
  );

  // Forget a session's stop request and clear its pending indicator. Used both
  // when a run ends and before a fresh run starts.
  const resetSessionStop = useCallback(
    (sessionId) => {
      const runtime = runtimeFor(sessionId);
      runtime.stopRequested = false;
      runtime.stopSentForActiveRun = false;
      setSessionStopPending(sessionId, false);
    },
    [runtimeFor, setSessionStopPending],
  );

  const setSessionModelInfo = useCallback(
    (sessionId, modelInfo) => {
      updateSessionState(sessionId, (state) => ({
        ...state,
        modelInfo: modelInfo ?? null,
      }));
    },
    [updateSessionState],
  );

  const setSessionStackAvailable = useCallback(
    (sessionId, available) => {
      updateSessionState(sessionId, (state) =>
        state.stackAvailable === available
          ? state
          : { ...state, stackAvailable: available },
      );
    },
    [updateSessionState],
  );

  const refreshStacks = useCallback(
    (sessionId) => {
      updateSessionState(sessionId, (state) => ({
        ...state,
        stackRefreshKey: state.stackRefreshKey + 1,
      }));
    },
    [updateSessionState],
  );

  const refreshStacksAfterTerminalEvent = useCallback(
    (sessionId) => {
      refreshStacks(sessionId);
      window.setTimeout(() => refreshStacks(sessionId), 120);
    },
    [refreshStacks],
  );

  const refreshModelInfo = useCallback(
    async (sessionId) => {
      if (!window.nav || !sessionId) {
        return;
      }
      // Guard against out-of-order resolution: a slow, stale fetch must not
      // overwrite the model a newer call (or an explicit switch) already showed.
      const runtime = runtimeFor(sessionId);
      const requestId = runtime.modelInfoRequest + 1;
      runtime.modelInfoRequest = requestId;
      try {
        const info = await window.nav.modelInfo(sessionId);
        if (requestId === runtime.modelInfoRequest) {
          setSessionModelInfo(sessionId, info ?? null);
        }
      } catch {
        // The indicator is best-effort; never let it disrupt the chat.
      }
    },
    [runtimeFor, setSessionModelInfo],
  );

  const refreshModelOptions = useCallback(async () => {
    if (!window.nav) {
      return;
    }
    try {
      setModelOptions(await window.nav.modelList());
    } catch {
      setModelOptions([]);
    }
  }, []);

  const refreshSessions = useCallback(async () => {
    if (!window.nav) {
      return;
    }
    try {
      setSessionSummaries(await window.nav.listSessions());
    } catch {
      // Listing is best-effort; never let it disrupt the chat.
    }
  }, []);

  const refreshStackAvailability = useCallback(
    async (sessionId, options = {}) => {
      if (!window.nav || !sessionId) {
        return;
      }
      const runtime = runtimeFor(sessionId);
      const requestId = runtime.stackRequest + 1;
      runtime.stackRequest = requestId;
      if (options.reset) {
        setSessionStackAvailable(sessionId, false);
      }
      try {
        const availability =
          await window.nav.sessionStackAvailability(sessionId);
        if (requestId === runtime.stackRequest) {
          setSessionStackAvailable(sessionId, availability?.available === true);
        }
      } catch {
        if (requestId === runtime.stackRequest) {
          setSessionStackAvailable(sessionId, false);
        }
      }
    },
    [runtimeFor, setSessionStackAvailable],
  );

  const refreshStackAvailabilityForEvent = useCallback(
    (sessionId, eventType) => {
      if (!shouldRefreshStackAvailabilityForEvent(eventType)) {
        return;
      }
      refreshStackAvailability(sessionId);
      // The terminal SSE event can beat the backend's JSONL stack append.
      window.setTimeout(
        () => refreshStackAvailability(sessionId),
        STACK_AVAILABILITY_RECHECK_DELAY_MS,
      );
    },
    [refreshStackAvailability],
  );

  // Surface a connection/backend error in a transcript the user can see. When
  // the failure names a session (e.g. one session's stream dropped), report it
  // there without yanking the user's active view; otherwise fall back to the
  // active session, or a synthetic one if the failure beat any real session.
  const reportStatusError = useCallback(
    (message, sessionId) => {
      if (sessionId) {
        appendSessionMessage(sessionId, "error", message);
        return;
      }
      const activeId = activeSessionIdRef.current;
      if (!activeId) {
        setActiveSession(SYSTEM_SESSION_ID);
      }
      appendSessionMessage(activeId ?? SYSTEM_SESSION_ID, "error", message);
    },
    [appendSessionMessage, setActiveSession],
  );

  const renderBackendStatus = useCallback(
    (status) => {
      switch (status.state) {
        case "starting-backend":
        case "connected":
          break;
        case "stream-error":
          reportStatusError(
            `Stream error: ${status.message}`,
            status.sessionId,
          );
          break;
        case "backend-error":
          reportStatusError(`Backend error: ${status.message}`);
          break;
        default:
          reportStatusError(status.message ?? status.state);
          break;
      }
    },
    [reportStatusError],
  );

  // Ask the backend to stop one session's in-flight run. Each session has its
  // own stop bookkeeping, so stopping one never touches another. A stop
  // requested before `run.started` arrives retries once when the run shows up.
  const stopRun = useCallback(
    async (sessionId) => {
      if (!window.nav || !sessionId) {
        return;
      }
      const runtime = runtimeFor(sessionId);
      if (runtime.stopRpcInFlight || runtime.stopSentForActiveRun) {
        return;
      }
      runtime.stopRequested = true;
      setSessionStopPending(sessionId, true);
      runtime.stopRpcInFlight = true;
      try {
        let shouldRetry = true;
        while (
          shouldRetry &&
          runtime.stopRequested &&
          !runtime.stopSentForActiveRun
        ) {
          const runHadStarted = runtime.runStarted;
          const stopped = await window.nav.sessionStop(sessionId);
          if (stopped) {
            runtime.stopSentForActiveRun = true;
          }
          shouldRetry = !stopped && !runHadStarted && runtime.runStarted;
        }
      } catch (error) {
        appendSessionMessage(
          sessionId,
          "error",
          `Could not stop: ${error.message}`,
        );
        setSessionStopPending(sessionId, false);
      } finally {
        runtime.stopRpcInFlight = false;
      }
    },
    [appendSessionMessage, runtimeFor, setSessionStopPending],
  );

  const handleBackendStatus = useCallback(
    (status) => {
      renderBackendStatus(status);
      if (status.state !== "connected") {
        return;
      }

      setConnectedState(true);
      if (status.sessionId) {
        setActiveSession(status.sessionId);
        refreshModelInfo(status.sessionId);
        refreshStackAvailability(status.sessionId, { reset: true });
      }
      refreshModelOptions();
      refreshSessions();
    },
    [
      refreshModelInfo,
      refreshModelOptions,
      refreshSessions,
      refreshStackAvailability,
      renderBackendStatus,
      setActiveSession,
      setConnectedState,
    ],
  );

  const handleSessionEvent = useCallback(
    (event) => {
      const sessionId = event.session_id;
      if (!sessionId) {
        return;
      }

      const runtime = runtimeFor(sessionId);
      // Skip events already applied so a backlog replay (the SSE feed resends a
      // session's full history when it re-subscribes after a stream error)
      // never re-appends the transcript or re-fires its side effects.
      if (event.event_id) {
        if (runtime.seenEventIds.has(event.event_id)) {
          return;
        }
        runtime.seenEventIds.add(event.event_id);
      }

      // Transcript + running/stopPending for the named session only.
      setSessionStates((current) => reduceSessions(current, event));

      switch (event.type) {
        case "run.started":
          runtime.runStarted = true;
          runtime.running = true;
          if (runtime.stopRequested) {
            stopRun(sessionId);
          }
          break;
        case "assistant.tool_calls":
          refreshModelInfo(sessionId);
          refreshStacks(sessionId);
          break;
        case "tool.completed":
        case "tool.failed":
          refreshStacks(sessionId);
          break;
        case "message.completed":
          refreshModelInfo(sessionId);
          refreshStacks(sessionId);
          break;
        case "run.completed":
          if (sessionId !== activeSessionIdRef.current) {
            setAttentionSessionIds((current) => {
              if (current.has(sessionId)) {
                return current;
              }
              const next = new Set(current);
              next.add(sessionId);
              return next;
            });
          }
          runtime.runStarted = false;
          runtime.running = false;
          runtime.stopRequested = false;
          runtime.stopSentForActiveRun = false;
          refreshSessions();
          refreshModelInfo(sessionId);
          refreshStackAvailabilityForEvent(sessionId, event.type);
          refreshStacksAfterTerminalEvent(sessionId);
          break;
        case "run.cancelled":
        case "run.failed":
          runtime.runStarted = false;
          runtime.running = false;
          runtime.stopRequested = false;
          runtime.stopSentForActiveRun = false;
          refreshSessions();
          refreshModelInfo(sessionId);
          refreshStackAvailabilityForEvent(sessionId, event.type);
          refreshStacksAfterTerminalEvent(sessionId);
          break;
        default:
          break;
      }
    },
    [
      refreshModelInfo,
      refreshSessions,
      refreshStackAvailabilityForEvent,
      refreshStacks,
      refreshStacksAfterTerminalEvent,
      runtimeFor,
      stopRun,
    ],
  );

  useNavSubscriptions(
    handleBackendStatus,
    handleSessionEvent,
    reportStatusError,
  );

  // Load the persisted "Start in" preference so the menu reflects the mode the
  // next launch will actually use (it is owned by Main, which reads it before
  // the renderer exists). Skip if the user already picked a mode, so a slow
  // async load can never clobber a fresh selection.
  useEffect(() => {
    let cancelled = false;
    window.nav
      ?.getSessionMode?.()
      .then((mode) => {
        if (
          !cancelled &&
          !sessionModeTouchedRef.current &&
          (mode === "local" || mode === "worktree")
        ) {
          setNewSessionMode(mode);
        }
      })
      .catch(() => {
        // Falling back to the default mode must never block the chat UI.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Persist every change so startup and new threads agree on the mode, even
  // across restarts.
  const changeNewSessionMode = useCallback((mode) => {
    sessionModeTouchedRef.current = true;
    setNewSessionMode(mode);
    window.nav?.setSessionMode?.(mode).catch(() => {
      // The in-memory selection still applies this session; persistence is a
      // best-effort convenience.
    });
  }, []);

  const activateCreatedSession = useCallback(
    async (sessionId) => {
      setActiveView("chat");
      setActiveSession(sessionId);
      refreshModelInfo(sessionId);
      refreshStackAvailability(sessionId, { reset: true });
      await refreshSessions();
    },
    [
      refreshModelInfo,
      refreshSessions,
      refreshStackAvailability,
      setActiveSession,
    ],
  );

  // Bring an existing session to the foreground. Switching is always allowed —
  // even while this or another session is running — because each session keeps
  // its own transcript and the backend streams them independently.
  const selectSession = useCallback(
    async (sessionId) => {
      if (sessionId === activeSessionIdRef.current || !connectedRef.current) {
        return;
      }

      const previousSessionId = activeSessionIdRef.current;
      setActiveView("chat");
      setActiveSession(sessionId);
      try {
        await window.nav.switchSession(sessionId);
        refreshModelInfo(sessionId);
        refreshStackAvailability(sessionId, { reset: true });
      } catch (error) {
        setActiveSession(previousSessionId);
        appendSessionMessage(
          previousSessionId ?? SYSTEM_SESSION_ID,
          "error",
          `Could not open session: ${error.message}`,
        );
      }
    },
    [
      appendSessionMessage,
      refreshModelInfo,
      refreshStackAvailability,
      setActiveSession,
    ],
  );

  const startNewChatInProject = useCallback(
    async (projectPath) => {
      if (!connectedRef.current || !window.nav) {
        return;
      }
      try {
        const sessionId = await window.nav.newSession(
          projectPath || null,
          newSessionMode,
        );
        if (!sessionId) {
          return;
        }
        await activateCreatedSession(sessionId);
      } catch (error) {
        reportStatusError(`Could not start a new chat: ${error.message}`);
      }
    },
    [activateCreatedSession, newSessionMode, reportStatusError],
  );

  const createProject = useCallback(async () => {
    if (!connectedRef.current || !window.nav) {
      return;
    }
    try {
      const sessionId = await window.nav.createProject(newSessionMode);
      if (!sessionId) {
        return;
      }
      await activateCreatedSession(sessionId);
    } catch (error) {
      reportStatusError(`Could not create project: ${error.message}`);
    }
  }, [activateCreatedSession, newSessionMode, reportStatusError]);

  const switchModel = useCallback(
    async (option) => {
      const sessionId = activeSessionIdRef.current;
      if (!connectedRef.current || !window.nav || !option) {
        return;
      }
      setModelSwitching(true);
      try {
        const info = await window.nav.switchModel(
          option.provider,
          option.model,
        );
        setSessionModelInfo(sessionId, info ?? null);
        await refreshModelInfo(sessionId);
      } catch (error) {
        reportStatusError(`Could not switch model: ${error.message}`);
      } finally {
        setModelSwitching(false);
      }
    },
    [refreshModelInfo, reportStatusError, setSessionModelInfo],
  );

  const switchThinking = useCallback(
    async (level) => {
      const sessionId = activeSessionIdRef.current;
      if (!connectedRef.current || !window.nav || !level) {
        return;
      }
      setModelSwitching(true);
      try {
        const info = await window.nav.switchThinking(level);
        setSessionModelInfo(sessionId, info ?? null);
        await refreshModelInfo(sessionId);
      } catch (error) {
        reportStatusError(`Could not switch thinking: ${error.message}`);
      } finally {
        setModelSwitching(false);
      }
    },
    [refreshModelInfo, reportStatusError, setSessionModelInfo],
  );

  const sendMessage = useCallback(
    async (text) => {
      const sessionId = activeSessionIdRef.current;
      if (!text || !connectedRef.current || !window.nav || !sessionId) {
        return;
      }

      const runtime = runtimeFor(sessionId);
      const wasRunning = runtime.running;
      if (!wasRunning) {
        runtime.runStarted = false;
        resetSessionStop(sessionId);
        setSessionRunning(sessionId, true);
      }

      try {
        await window.nav.sessionSendMessage(sessionId, text);
      } catch (error) {
        appendSessionMessage(
          sessionId,
          "error",
          `Could not send message: ${error.message}`,
        );
        if (!wasRunning) {
          setSessionRunning(sessionId, false);
        }
      }
    },
    [appendSessionMessage, resetSessionStop, runtimeFor, setSessionRunning],
  );

  const activeState = sessionStates[activeSessionId] ?? EMPTY_SESSION_STATE;

  const runningSessionIds = useMemo(() => {
    const ids = new Set();
    for (const [id, state] of Object.entries(sessionStates)) {
      if (state.running) {
        ids.add(id);
      }
    }
    return ids;
  }, [sessionStates]);

  const activeProjectPath = useMemo(
    () =>
      sessionSummaries.find((session) => session.sessionId === activeSessionId)
        ?.workspaceRoot ?? null,
    [activeSessionId, sessionSummaries],
  );

  const markStacksUnavailable = useCallback(() => {
    setSessionStackAvailable(activeSessionIdRef.current, false);
  }, [setSessionStackAvailable]);

  return (
    <div className="app">
      <Sidebar
        activeSessionId={activeSessionId}
        attentionSessionIds={attentionSessionIds}
        connected={connected}
        runningSessionIds={runningSessionIds}
        sessions={sessionSummaries}
        onCreateProject={createProject}
        onNewChat={() => startNewChatInProject(activeProjectPath)}
        onNewChatInProject={startNewChatInProject}
        onSelectSession={selectSession}
      />
      <main className="shell">
        <SessionToolbar
          activeView={activeView}
          connected={connected}
          running={activeState.running}
          sessionId={activeSessionId}
          showStacks={activeState.stackAvailable}
          onSelectView={setActiveView}
        />
        {activeView === "stacks" ? (
          <StacksPage
            key={`${activeSessionId ?? "none"}-${activeState.stackRefreshKey}`}
            onUnavailable={markStacksUnavailable}
            sessionId={activeSessionId}
          />
        ) : (
          <>
            <Transcript
              key={activeSessionId ?? "none"}
              messages={activeState.messages}
            />
            <Composer
              connected={connected}
              modelInfo={activeState.modelInfo}
              modelOptions={modelOptions}
              modelSwitching={modelSwitching}
              newSessionMode={newSessionMode}
              running={activeState.running}
              stopPending={activeState.stopPending}
              onNewSessionModeChange={changeNewSessionMode}
              onModelChange={switchModel}
              onThinkingChange={switchThinking}
              onSend={sendMessage}
              onStop={() => stopRun(activeSessionIdRef.current)}
            />
          </>
        )}
      </main>
    </div>
  );
}

function SessionToolbar({
  activeView,
  connected,
  running,
  sessionId,
  showStacks,
  onSelectView,
}) {
  return (
    <header className="session-toolbar">
      <div className="session-toolbar-title">
        <span className="session-toolbar-label">Thread</span>
        <span className="session-toolbar-id">
          {sessionId ? shortId(sessionId) : "none"}
        </span>
        {running ? (
          <span
            className="session-toolbar-running"
            role="img"
            aria-label="Running"
            title="Running"
          />
        ) : null}
      </div>
      <nav className="session-view-tabs" aria-label="Thread views">
        <button
          type="button"
          className="session-view-tab"
          aria-current={activeView === "chat" ? "page" : undefined}
          onClick={() => onSelectView("chat")}
        >
          Chat
        </button>
        {showStacks ? (
          <button
            type="button"
            className="session-view-tab"
            aria-current={activeView === "stacks" ? "page" : undefined}
            disabled={!connected || !sessionId}
            onClick={() => onSelectView("stacks")}
          >
            Stacks
          </button>
        ) : null}
      </nav>
    </header>
  );
}

function shortId(id) {
  return id.length > 8 ? id.slice(0, 8) : id;
}

function useNavSubscriptions(handleBackendStatus, handleSessionEvent, onError) {
  useLayoutEffect(() => {
    if (window.nav) {
      const unsubscribeStatus = window.nav.onBackendStatus(handleBackendStatus);
      const unsubscribeEvents = window.nav.onSessionEvent(handleSessionEvent);
      return () => {
        unsubscribeStatus?.();
        unsubscribeEvents?.();
      };
    }

    onError("Electron preload API unavailable");
    return undefined;
  }, [handleBackendStatus, handleSessionEvent, onError]);
}
