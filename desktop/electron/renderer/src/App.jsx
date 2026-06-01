import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import Composer from "./components/Composer.jsx";
import Sidebar from "./components/Sidebar.jsx";
import StacksPage from "./components/StacksPage.jsx";
import Transcript from "./components/Transcript.jsx";

export default function App() {
  const [connected, setConnected] = useState(false);
  const [running, setRunning] = useState(false);
  const [activeSessionId, setActiveSessionId] = useState(null);
  const [sessionSummaries, setSessionSummaries] = useState([]);
  const [messages, setMessages] = useState([]);
  const [modelInfo, setModelInfo] = useState(null);
  const [modelOptions, setModelOptions] = useState([]);
  const [modelSwitching, setModelSwitching] = useState(false);
  const [stopPending, setStopPending] = useState(false);
  const [activeView, setActiveView] = useState("chat");
  const [stackAvailable, setStackAvailable] = useState(false);
  const [stackRefreshKey, setStackRefreshKey] = useState(0);
  const [newSessionMode, setNewSessionMode] = useState("local");

  const connectedRef = useRef(false);
  const runningRef = useRef(false);
  const activeSessionIdRef = useRef(null);
  const activeRunStartedRef = useRef(false);
  const stopRpcInFlightRef = useRef(false);
  const stopSentForActiveRunRef = useRef(false);
  const stopRequestedRef = useRef(false);
  const nextMessageIdRef = useRef(0);
  const stackAvailabilityRequestRef = useRef(0);

  const setConnectedState = useCallback((isConnected) => {
    connectedRef.current = isConnected;
    setConnected(isConnected);
  }, []);

  const setRunningState = useCallback((isRunning) => {
    runningRef.current = isRunning;
    setRunning(isRunning);
  }, []);

  const setActiveSession = useCallback((sessionId) => {
    activeSessionIdRef.current = sessionId;
    setActiveSessionId(sessionId);
  }, []);

  const setStopPendingState = useCallback((isPending) => {
    setStopPending(isPending);
  }, []);

  const setStopRequested = useCallback(
    (isRequested) => {
      stopRequestedRef.current = isRequested;
      if (!isRequested) {
        stopSentForActiveRunRef.current = false;
        setStopPendingState(false);
      }
    },
    [setStopPendingState],
  );

  const nextMessageId = useCallback(() => {
    nextMessageIdRef.current += 1;
    return `message-${nextMessageIdRef.current}`;
  }, []);

  const refreshStacks = useCallback(() => {
    setStackRefreshKey((key) => key + 1);
  }, []);

  const refreshStacksAfterTerminalEvent = useCallback(() => {
    refreshStacks();
    window.setTimeout(refreshStacks, 120);
  }, [refreshStacks]);

  const markStacksUnavailable = useCallback(() => {
    setStackAvailable(false);
  }, []);

  const appendMessage = useCallback(
    (role, text) => {
      setMessages((current) => [
        ...current,
        {
          id: nextMessageId(),
          role,
          text: text ?? "",
          createdAt: new Date().toISOString(),
        },
      ]);
    },
    [nextMessageId],
  );

  const clearTranscript = useCallback(() => {
    setMessages([]);
  }, []);

  const upsertToolLine = useCallback(
    (toolCallId, state, toolName, detail) => {
      setMessages((current) => {
        const nextToolLine = (existingId) => ({
          id: existingId ?? nextMessageId(),
          role: "tool",
          toolCallId,
          state,
          toolName: toolName ?? "tool",
          detail: detail ?? "",
        });

        if (!toolCallId) {
          return [...current, nextToolLine()];
        }

        const index = current.findIndex(
          (message) =>
            message.role === "tool" && message.toolCallId === toolCallId,
        );
        if (index === -1) {
          return [...current, nextToolLine()];
        }

        const next = current.slice();
        next[index] = nextToolLine(current[index].id);
        return next;
      });
    },
    [nextMessageId],
  );

  const refreshModelInfo = useCallback(async (sessionId) => {
    if (!window.nav) {
      return;
    }
    const targetSessionId = sessionId ?? activeSessionIdRef.current;
    try {
      const info = await window.nav.modelInfo(targetSessionId);
      if ((targetSessionId ?? null) === (activeSessionIdRef.current ?? null)) {
        setModelInfo(info ?? null);
      }
    } catch {
      // The indicator is best-effort; never let it disrupt the chat.
    }
  }, []);

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
      if (!window.nav) {
        return;
      }
      const targetSessionId = sessionId ?? activeSessionIdRef.current;
      if (!targetSessionId) {
        setStackAvailable(false);
        return;
      }
      const requestId = stackAvailabilityRequestRef.current + 1;
      stackAvailabilityRequestRef.current = requestId;
      if (options.reset) {
        setStackAvailable(false);
      }
      try {
        const availability =
          await window.nav.sessionStackAvailability(targetSessionId);
        if (
          requestId === stackAvailabilityRequestRef.current &&
          targetSessionId === activeSessionIdRef.current
        ) {
          setStackAvailable(availability?.available === true);
        }
      } catch {
        if (
          requestId === stackAvailabilityRequestRef.current &&
          targetSessionId === activeSessionIdRef.current
        ) {
          setStackAvailable(false);
        }
      }
    },
    [],
  );

  const renderBackendStatus = useCallback(
    (status) => {
      switch (status.state) {
        case "starting-backend":
        case "connected":
          break;
        case "stream-error":
          appendMessage("error", `Stream error: ${status.message}`);
          break;
        case "backend-error":
          appendMessage("error", `Backend error: ${status.message}`);
          break;
        default:
          appendMessage("error", status.message ?? status.state);
          break;
      }
    },
    [appendMessage],
  );

  const stopRun = useCallback(async () => {
    if (
      !window.nav ||
      stopRpcInFlightRef.current ||
      stopSentForActiveRunRef.current
    ) {
      return;
    }
    stopRequestedRef.current = true;
    setStopPendingState(true);
    stopRpcInFlightRef.current = true;
    try {
      // A pre-registration stop can race with run.started; retry once if the
      // backend reported no active run before that event arrived.
      let shouldRetry = true;
      while (
        shouldRetry &&
        stopRequestedRef.current &&
        !stopSentForActiveRunRef.current
      ) {
        const runHadStarted = activeRunStartedRef.current;
        const stopped = await window.nav.sessionStop();
        if (stopped) {
          stopSentForActiveRunRef.current = true;
        }
        shouldRetry = !stopped && !runHadStarted && activeRunStartedRef.current;
      }
    } catch (error) {
      appendMessage("error", `Could not stop: ${error.message}`);
      setStopPendingState(false);
    } finally {
      stopRpcInFlightRef.current = false;
    }
  }, [appendMessage, setStopPendingState]);

  const handleBackendStatus = useCallback(
    (status) => {
      renderBackendStatus(status);
      if (status.state !== "connected") {
        return;
      }

      setConnectedState(true);
      if (status.sessionId) {
        setActiveSession(status.sessionId);
      }
      setRunningState(false);
      refreshModelOptions();
      refreshSessions();
      refreshModelInfo(status.sessionId);
      refreshStackAvailability(status.sessionId, { reset: true });
    },
    [
      refreshModelInfo,
      refreshModelOptions,
      refreshSessions,
      refreshStackAvailability,
      renderBackendStatus,
      setActiveSession,
      setConnectedState,
      setRunningState,
    ],
  );

  const handleSessionEvent = useCallback(
    (event) => {
      switch (event.type) {
        case "user.message":
          appendMessage("user", event.text);
          break;
        case "run.started":
          activeRunStartedRef.current = true;
          setRunningState(true);
          if (stopRequestedRef.current) {
            stopRun();
          }
          break;
        case "assistant.tool_calls":
          if (event.text) {
            appendMessage("assistant", event.text);
          }
          refreshStacks();
          break;
        case "tool.started":
          upsertToolLine(event.tool_call_id, "running", event.tool_name);
          break;
        case "tool.completed":
          upsertToolLine(
            event.tool_call_id,
            "done",
            event.tool_name,
            event.text,
          );
          refreshStacks();
          break;
        case "tool.failed":
          upsertToolLine(
            event.tool_call_id,
            "failed",
            event.tool_name,
            event.error,
          );
          refreshStacks();
          break;
        case "message.completed":
          appendMessage("assistant", event.text);
          refreshStacks();
          break;
        case "run.completed":
        case "run.cancelled":
          activeRunStartedRef.current = false;
          setRunningState(false);
          setStopRequested(false);
          refreshSessions();
          refreshModelInfo();
          refreshStackAvailability();
          refreshStacksAfterTerminalEvent();
          break;
        case "run.failed":
          appendMessage("error", event.error ?? "the run failed");
          activeRunStartedRef.current = false;
          setRunningState(false);
          setStopRequested(false);
          refreshSessions();
          refreshModelInfo();
          refreshStackAvailability();
          refreshStacksAfterTerminalEvent();
          break;
        default:
          break;
      }
    },
    [
      appendMessage,
      refreshModelInfo,
      refreshSessions,
      refreshStackAvailability,
      refreshStacks,
      refreshStacksAfterTerminalEvent,
      setRunningState,
      setStopRequested,
      stopRun,
      upsertToolLine,
    ],
  );

  useNavSubscriptions(handleBackendStatus, handleSessionEvent, appendMessage);

  const activateCreatedSession = useCallback(
    async (sessionId) => {
      setActiveSession(sessionId);
      setStackAvailable(false);
      await refreshSessions();
      refreshModelInfo(sessionId);
      refreshStackAvailability(sessionId, { reset: true });
    },
    [
      refreshModelInfo,
      refreshSessions,
      refreshStackAvailability,
      setActiveSession,
    ],
  );

  const selectSession = useCallback(
    async (sessionId) => {
      if (
        sessionId === activeSessionIdRef.current ||
        runningRef.current ||
        !connectedRef.current
      ) {
        return;
      }

      const previousSessionId = activeSessionIdRef.current;
      clearTranscript();
      setActiveView("chat");
      setStackAvailable(false);
      setActiveSession(sessionId);
      try {
        await window.nav.switchSession(sessionId);
        refreshModelInfo(sessionId);
        refreshStackAvailability(sessionId, { reset: true });
      } catch (error) {
        setActiveSession(previousSessionId);
        refreshStackAvailability(previousSessionId, { reset: true });
        appendMessage("error", `Could not open session: ${error.message}`);
      }
    },
    [
      appendMessage,
      clearTranscript,
      refreshModelInfo,
      refreshStackAvailability,
      setActiveSession,
    ],
  );

  const startNewChatInProject = useCallback(
    async (projectPath) => {
      if (runningRef.current || !connectedRef.current || !window.nav) {
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
        clearTranscript();
        await activateCreatedSession(sessionId);
      } catch (error) {
        appendMessage("error", `Could not start a new chat: ${error.message}`);
      }
    },
    [activateCreatedSession, appendMessage, clearTranscript, newSessionMode],
  );

  const createProject = useCallback(async () => {
    if (runningRef.current || !connectedRef.current || !window.nav) {
      return;
    }
    try {
      const sessionId = await window.nav.createProject(newSessionMode);
      if (!sessionId) {
        return;
      }
      clearTranscript();
      await activateCreatedSession(sessionId);
    } catch (error) {
      appendMessage("error", `Could not create project: ${error.message}`);
    }
  }, [activateCreatedSession, appendMessage, clearTranscript, newSessionMode]);

  const switchModel = useCallback(
    async (option) => {
      if (!connectedRef.current || !window.nav || !option) {
        return;
      }
      setModelSwitching(true);
      try {
        const info = await window.nav.switchModel(
          option.provider,
          option.model,
        );
        setModelInfo(info ?? null);
        await refreshModelInfo();
      } catch (error) {
        appendMessage("error", `Could not switch model: ${error.message}`);
      } finally {
        setModelSwitching(false);
      }
    },
    [appendMessage, refreshModelInfo],
  );

  const switchThinking = useCallback(
    async (level) => {
      if (!connectedRef.current || !window.nav || !level) {
        return;
      }
      setModelSwitching(true);
      try {
        const info = await window.nav.switchThinking(level);
        setModelInfo(info ?? null);
        await refreshModelInfo();
      } catch (error) {
        appendMessage("error", `Could not switch thinking: ${error.message}`);
      } finally {
        setModelSwitching(false);
      }
    },
    [appendMessage, refreshModelInfo],
  );

  const sendMessage = useCallback(
    async (text) => {
      if (!text || !connectedRef.current || !window.nav) {
        return;
      }

      const wasRunning = runningRef.current;
      if (!wasRunning) {
        activeRunStartedRef.current = false;
        setStopRequested(false);
        setRunningState(true);
      }

      try {
        await window.nav.sessionSendMessage(text);
      } catch (error) {
        appendMessage("error", `Could not send message: ${error.message}`);
        if (!wasRunning) {
          setRunningState(false);
        }
      }
    },
    [appendMessage, setRunningState, setStopRequested],
  );

  const activeProjectPath = useMemo(
    () =>
      sessionSummaries.find((session) => session.sessionId === activeSessionId)
        ?.workspaceRoot ?? null,
    [activeSessionId, sessionSummaries],
  );

  return (
    <div className="app">
      <Sidebar
        activeSessionId={activeSessionId}
        connected={connected}
        running={running}
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
          sessionId={activeSessionId}
          showStacks={stackAvailable}
          onSelectView={setActiveView}
        />
        {activeView === "stacks" ? (
          <StacksPage
            key={`${activeSessionId ?? "none"}-${stackRefreshKey}`}
            onUnavailable={markStacksUnavailable}
            sessionId={activeSessionId}
          />
        ) : (
          <>
            <Transcript messages={messages} />
            <Composer
              connected={connected}
              modelInfo={modelInfo}
              modelOptions={modelOptions}
              modelSwitching={modelSwitching}
              newSessionMode={newSessionMode}
              running={running}
              stopPending={stopPending}
              onNewSessionModeChange={setNewSessionMode}
              onModelChange={switchModel}
              onThinkingChange={switchThinking}
              onSend={sendMessage}
              onStop={stopRun}
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

    onError("error", "Electron preload API unavailable");
    return undefined;
  }, [handleBackendStatus, handleSessionEvent, onError]);
}
