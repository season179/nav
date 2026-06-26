import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
  Fragment,
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import Composer from "./components/Composer.tsx";
import Sidebar from "./components/Sidebar.tsx";
import { Tabs, TabsList, TabsTrigger } from "./components/ui/tabs.tsx";
import { type NavAppView, parseNavPathname } from "./lib/app-routes.ts";
import {
  modelOptionsQueryOptions,
  navQueryKeys,
  navSessionsQueryOptions,
} from "./lib/nav-queries.ts";
import { createSessionState } from "./lib/session-runtime.ts";
import {
  appendStoredSessionMessage,
  applyStoredSessionEvent,
  updateStoredSessionState,
  useSessionStates,
} from "./lib/session-store.ts";
import {
  STACK_AVAILABILITY_RECHECK_DELAY_MS,
  shouldRefreshStackAvailabilityForEvent,
} from "./lib/stack-availability.ts";
import { cn } from "./lib/utils.ts";
import type {
  BackendStatus,
  ChatMessage,
  ModelInfo,
  ModelOption,
  SessionEvent,
  SessionMode,
} from "./types.ts";

const SettingsPage = lazy(() => import("./components/SettingsPage.tsx"));
const StacksPage = lazy(() => import("./components/StacksPage.tsx"));
const Transcript = lazy(() => import("./components/Transcript.tsx"));

// Shown when a session has no state yet (e.g. before its first event arrives),
// so the composer and transcript always have a stable object to read.
const EMPTY_SESSION_STATE = createSessionState();
// Where startup/connection errors land before any real session exists.
const SYSTEM_SESSION_ID = "system";

type ViewName = Extract<NavAppView, "chat" | "stacks" | "settings">;

// Imperative per-session run bookkeeping read synchronously by the stop/send
// paths (React state would be stale inside the async stop loop). `running`
// mirrors the view state's flag; the rest track one in-flight run.
type SessionRuntime = {
  running: boolean;
  runStarted: boolean;
  stopRequested: boolean;
  stopRpcInFlight: boolean;
  stopSentForActiveRun: boolean;
  stackRequest: number;
  modelInfoRequest: number;
  seenEventIds: Set<string>;
};

export default function App() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const routePathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const routeState = useMemo(
    () => parseNavPathname(routePathname),
    [routePathname],
  );
  const [connected, setConnected] = useState(false);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [attentionSessionIds, setAttentionSessionIds] = useState(
    () => new Set<string>(),
  );
  const [modelSwitching, setModelSwitching] = useState(false);
  const [newSessionMode, setNewSessionMode] = useState<SessionMode>("local");

  const connectedRef = useRef(false);
  const activeSessionIdRef = useRef<string | null>(null);
  const runtimesRef = useRef<Map<string, SessionRuntime>>(new Map());
  const sessionModeTouchedRef = useRef(false);

  const sessionsQuery = useQuery({
    ...navSessionsQueryOptions(),
    enabled: connected,
  });
  const modelOptionsQuery = useQuery({
    ...modelOptionsQueryOptions(),
    enabled: connected,
  });
  const sessionStates = useSessionStates((states) => states);
  const sessionSummaries = sessionsQuery.data ?? [];
  const modelOptions = modelOptionsQuery.data ?? [];
  const activeView: ViewName =
    routeState.view === "stacks"
      ? "stacks"
      : routeState.view === "settings"
        ? "settings"
        : "chat";

  const setConnectedState = useCallback((isConnected: boolean) => {
    connectedRef.current = isConnected;
    setConnected(isConnected);
  }, []);

  const setActiveSession = useCallback((sessionId: string | null) => {
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

  const navigateToView = useCallback(
    (
      view: ViewName,
      sessionId: string | null | undefined,
      options: { replace?: boolean } = {},
    ) => {
      if (view === "settings") {
        if (sessionId) {
          void navigate({
            params: { sessionId },
            replace: options.replace,
            to: "/sessions/$sessionId/settings",
          });
          return;
        }
        void navigate({ replace: options.replace, to: "/settings" });
        return;
      }
      if (view === "stacks" && sessionId) {
        void navigate({
          params: { sessionId },
          replace: options.replace,
          to: "/sessions/$sessionId/stacks",
        });
        return;
      }
      if (sessionId) {
        void navigate({
          params: { sessionId },
          replace: options.replace,
          to: "/sessions/$sessionId",
        });
        return;
      }
      void navigate({ replace: options.replace, to: "/chat" });
    },
    [navigate],
  );

  const runtimeFor = useCallback((sessionId: string): SessionRuntime => {
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

  const appendSessionMessage = useCallback(
    (sessionId: string, role: ChatMessage["role"], text?: string) => {
      appendStoredSessionMessage(sessionId, role, text);
    },
    [],
  );

  const setSessionRunning = useCallback(
    (sessionId: string, isRunning: boolean) => {
      runtimeFor(sessionId).running = isRunning;
      updateStoredSessionState(sessionId, (state) =>
        state.running === isRunning ? state : { ...state, running: isRunning },
      );
    },
    [runtimeFor],
  );

  const setSessionStopPending = useCallback(
    (sessionId: string, isPending: boolean) => {
      updateStoredSessionState(sessionId, (state) =>
        state.stopPending === isPending
          ? state
          : { ...state, stopPending: isPending },
      );
    },
    [],
  );

  // Forget a session's stop request and clear its pending indicator. Used both
  // when a run ends and before a fresh run starts.
  const resetSessionStop = useCallback(
    (sessionId: string) => {
      const runtime = runtimeFor(sessionId);
      runtime.stopRequested = false;
      runtime.stopSentForActiveRun = false;
      setSessionStopPending(sessionId, false);
    },
    [runtimeFor, setSessionStopPending],
  );

  const setSessionModelInfo = useCallback(
    (sessionId: string | null, modelInfo: ModelInfo | null) => {
      updateStoredSessionState(sessionId, (state) => ({
        ...state,
        modelInfo: modelInfo ?? null,
      }));
    },
    [],
  );

  const setSessionStackAvailable = useCallback(
    (sessionId: string | null, available: boolean) => {
      updateStoredSessionState(sessionId, (state) =>
        state.stackAvailable === available
          ? state
          : { ...state, stackAvailable: available },
      );
    },
    [],
  );

  const refreshStacks = useCallback(
    (sessionId: string) => {
      void queryClient.invalidateQueries({
        queryKey: navQueryKeys.stacks(sessionId),
      });
    },
    [queryClient],
  );

  const refreshStacksAfterTerminalEvent = useCallback(
    (sessionId: string) => {
      refreshStacks(sessionId);
      window.setTimeout(() => refreshStacks(sessionId), 120);
    },
    [refreshStacks],
  );

  const refreshModelInfo = useCallback(
    async (sessionId: string | null | undefined) => {
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

  const refreshSessions = useCallback(async () => {
    await queryClient.invalidateQueries({ queryKey: navQueryKeys.sessions() });
  }, [queryClient]);

  const refreshStackAvailability = useCallback(
    async (
      sessionId: string | null | undefined,
      options: { reset?: boolean } = {},
    ) => {
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
    (sessionId: string, eventType: string) => {
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

  useEffect(() => {
    if (!routeState.known) {
      navigateToView("chat", null, { replace: true });
      return;
    }

    if (routeState.sessionId !== activeSessionIdRef.current) {
      setActiveSession(routeState.sessionId);
      if (routeState.sessionId && connectedRef.current) {
        window.nav?.switchSession(routeState.sessionId).catch((error) => {
          appendSessionMessage(
            routeState.sessionId ?? SYSTEM_SESSION_ID,
            "error",
            `Could not open session: ${errorMessage(error)}`,
          );
        });
        refreshModelInfo(routeState.sessionId);
        refreshStackAvailability(routeState.sessionId, { reset: true });
      }
    }
  }, [
    appendSessionMessage,
    navigateToView,
    refreshModelInfo,
    refreshStackAvailability,
    routeState.known,
    routeState.sessionId,
    setActiveSession,
  ]);

  // Surface a connection/backend error in a transcript the user can see. When
  // the failure names a session (e.g. one session's stream dropped), report it
  // there without yanking the user's active view; otherwise fall back to the
  // active session, or a synthetic one if the failure beat any real session.
  const reportStatusError = useCallback(
    (message: string, sessionId?: string) => {
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
    (status: BackendStatus) => {
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
    async (sessionId: string | null) => {
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
          `Could not stop: ${errorMessage(error)}`,
        );
        setSessionStopPending(sessionId, false);
      } finally {
        runtime.stopRpcInFlight = false;
      }
    },
    [appendSessionMessage, runtimeFor, setSessionStopPending],
  );

  const handleBackendStatus = useCallback(
    (status: BackendStatus) => {
      renderBackendStatus(status);
      if (status.state !== "connected") {
        return;
      }

      setConnectedState(true);
      const sessionId = routeState.sessionId ?? status.sessionId;
      if (sessionId) {
        setActiveSession(sessionId);
        if (routeState.sessionId && routeState.sessionId !== status.sessionId) {
          window.nav?.switchSession(routeState.sessionId).catch((error) => {
            appendSessionMessage(
              routeState.sessionId ?? SYSTEM_SESSION_ID,
              "error",
              `Could not open session: ${errorMessage(error)}`,
            );
          });
        }
        if (!routeState.sessionId) {
          navigateToView("chat", sessionId, { replace: true });
        }
        refreshModelInfo(sessionId);
        refreshStackAvailability(sessionId, { reset: true });
      }
      void queryClient.invalidateQueries({
        queryKey: navQueryKeys.modelOptions(),
      });
      void refreshSessions();
    },
    [
      appendSessionMessage,
      refreshModelInfo,
      refreshSessions,
      refreshStackAvailability,
      navigateToView,
      queryClient,
      renderBackendStatus,
      routeState.sessionId,
      setActiveSession,
      setConnectedState,
    ],
  );

  const handleSessionEvent = useCallback(
    (event: SessionEvent) => {
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
      applyStoredSessionEvent(event);

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
  const changeNewSessionMode = useCallback((mode: SessionMode) => {
    sessionModeTouchedRef.current = true;
    setNewSessionMode(mode);
    window.nav?.setSessionMode?.(mode).catch(() => {
      // The in-memory selection still applies this session; persistence is a
      // best-effort convenience.
    });
  }, []);

  const activateCreatedSession = useCallback(
    async (sessionId: string) => {
      setActiveSession(sessionId);
      navigateToView("chat", sessionId);
      refreshModelInfo(sessionId);
      refreshStackAvailability(sessionId, { reset: true });
      await refreshSessions();
    },
    [
      refreshModelInfo,
      refreshSessions,
      refreshStackAvailability,
      navigateToView,
      setActiveSession,
    ],
  );

  // Bring an existing session to the foreground. Switching is always allowed —
  // even while this or another session is running — because each session keeps
  // its own transcript and the backend streams them independently.
  const selectSession = useCallback(
    async (sessionId: string) => {
      if (sessionId === activeSessionIdRef.current || !connectedRef.current) {
        return;
      }

      const previousSessionId = activeSessionIdRef.current;
      setActiveSession(sessionId);
      navigateToView("chat", sessionId);
      try {
        await window.nav.switchSession(sessionId);
        refreshModelInfo(sessionId);
        refreshStackAvailability(sessionId, { reset: true });
      } catch (error) {
        setActiveSession(previousSessionId);
        navigateToView("chat", previousSessionId, { replace: true });
        appendSessionMessage(
          previousSessionId ?? SYSTEM_SESSION_ID,
          "error",
          `Could not open session: ${errorMessage(error)}`,
        );
      }
    },
    [
      appendSessionMessage,
      navigateToView,
      refreshModelInfo,
      refreshStackAvailability,
      setActiveSession,
    ],
  );

  const startNewChatInProject = useCallback(
    async (projectPath: string | null) => {
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
        reportStatusError(`Could not start a new chat: ${errorMessage(error)}`);
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
      reportStatusError(`Could not create project: ${errorMessage(error)}`);
    }
  }, [activateCreatedSession, newSessionMode, reportStatusError]);

  const switchModel = useCallback(
    async (option: ModelOption) => {
      const sessionId = activeSessionIdRef.current;
      if (!connectedRef.current || !window.nav || !option || !sessionId) {
        return;
      }
      setModelSwitching(true);
      try {
        const info = await window.nav.switchModel(
          sessionId,
          option.provider,
          option.model,
        );
        setSessionModelInfo(sessionId, info ?? null);
        await refreshModelInfo(sessionId);
      } catch (error) {
        reportStatusError(`Could not switch model: ${errorMessage(error)}`);
      } finally {
        setModelSwitching(false);
      }
    },
    [refreshModelInfo, reportStatusError, setSessionModelInfo],
  );

  const switchThinking = useCallback(
    async (level: string) => {
      const sessionId = activeSessionIdRef.current;
      if (!connectedRef.current || !window.nav || !level || !sessionId) {
        return;
      }
      setModelSwitching(true);
      try {
        const info = await window.nav.switchThinking(sessionId, level);
        setSessionModelInfo(sessionId, info ?? null);
        await refreshModelInfo(sessionId);
      } catch (error) {
        reportStatusError(`Could not switch thinking: ${errorMessage(error)}`);
      } finally {
        setModelSwitching(false);
      }
    },
    [refreshModelInfo, reportStatusError, setSessionModelInfo],
  );

  const sendMessage = useCallback(
    async (text: string) => {
      const sessionId = activeSessionIdRef.current;
      if (!text || !connectedRef.current || !window.nav || !sessionId) {
        return;
      }

      const runtime = runtimeFor(sessionId);
      const wasRunning = runtime.running;
      if (!wasRunning) {
        runtime.runStarted = false;
        resetSessionStop(sessionId);
        appendSessionMessage(sessionId, "user", text);
        setSessionRunning(sessionId, true);
      }

      try {
        await window.nav.sessionSendMessage(sessionId, text);
      } catch (error) {
        appendSessionMessage(
          sessionId,
          "error",
          `Could not send message: ${errorMessage(error)}`,
        );
        if (!wasRunning) {
          setSessionRunning(sessionId, false);
        }
      }
    },
    [appendSessionMessage, resetSessionStop, runtimeFor, setSessionRunning],
  );

  const activeState =
    (activeSessionId ? sessionStates[activeSessionId] : null) ??
    EMPTY_SESSION_STATE;

  const runningSessionIds = useMemo(() => {
    const ids = new Set<string>();
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

  const selectView = useCallback(
    (view: ViewName) => {
      navigateToView(view, activeSessionIdRef.current);
    },
    [navigateToView],
  );

  return (
    <div className="grid h-screen min-h-0 grid-cols-[20rem_minmax(0,1fr)] overflow-hidden bg-background text-foreground">
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
      <main className="flex min-h-0 flex-col overflow-hidden">
        <SessionToolbar
          activeView={activeView}
          connected={connected}
          running={activeState.running}
          sessionId={activeSessionId}
          showStacks={activeState.stackAvailable}
          onSelectView={selectView}
        />
        <Suspense fallback={null}>
          {activeView === "settings" ? (
            <SettingsPage
              connected={connected}
              modelInfo={activeState.modelInfo}
              modelOptions={modelOptions}
              modelSwitching={modelSwitching}
              newSessionMode={newSessionMode}
              sessionId={activeSessionId}
              onModelChange={switchModel}
              onNewSessionModeChange={changeNewSessionMode}
              onThinkingChange={switchThinking}
            />
          ) : activeView === "stacks" ? (
            <StacksPage
              key={activeSessionId ?? "none"}
              onUnavailable={markStacksUnavailable}
              sessionId={activeSessionId}
            />
          ) : (
            <Fragment key={activeSessionId ?? "pending"}>
              {activeSessionId ? (
                <Transcript messages={activeState.messages} />
              ) : (
                <ChatPlaceholder />
              )}
              <Composer
                key={activeSessionId ?? "none"}
                connected={connected}
                draftKey={activeSessionId}
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
            </Fragment>
          )}
        </Suspense>
      </main>
    </div>
  );
}

function ChatPlaceholder() {
  return (
    <section
      className="flex min-h-0 flex-1 items-center justify-center bg-background px-5 text-center"
      aria-label="Chat transcript"
    >
      <div className="space-y-1 text-muted-foreground text-sm">
        <h2 className="font-medium text-foreground text-sm">Starting nav</h2>
        <p>Preparing the thread.</p>
      </div>
    </section>
  );
}

function SessionToolbar({
  activeView,
  connected,
  running,
  sessionId,
  showStacks,
  onSelectView,
}: {
  activeView: ViewName;
  connected: boolean;
  running: boolean;
  sessionId: string | null;
  showStacks: boolean;
  onSelectView: (view: ViewName) => void;
}) {
  function handleViewChange(value: string) {
    if (value === "chat" || value === "stacks" || value === "settings") {
      onSelectView(value);
    }
  }

  return (
    <header className="flex h-14 shrink-0 items-center justify-between border-b bg-background/95 px-5">
      <div className="flex min-w-0 items-center gap-2">
        <span className="font-medium text-muted-foreground text-xs uppercase tracking-[0.12em]">
          Thread
        </span>
        <span className="rounded-md bg-muted px-2 py-1 font-mono text-muted-foreground text-xs">
          {sessionId ? shortId(sessionId) : "none"}
        </span>
        {running ? (
          <span
            className="size-2 rounded-full bg-primary"
            role="img"
            aria-label="Running"
            title="Running"
          />
        ) : null}
      </div>
      <Tabs
        className="shrink-0"
        value={activeView}
        onValueChange={handleViewChange}
      >
        <TabsList aria-label="Thread views" variant="line">
          <TabsTrigger
            className={cn(activeView === "chat" && "text-foreground")}
            value="chat"
          >
            Chat
          </TabsTrigger>
          {showStacks ? (
            <TabsTrigger
              className={cn(activeView === "stacks" && "text-foreground")}
              disabled={!connected || !sessionId}
              value="stacks"
            >
              Stacks
            </TabsTrigger>
          ) : null}
          <TabsTrigger
            className={cn(activeView === "settings" && "text-foreground")}
            disabled={!connected}
            value="settings"
          >
            Settings
          </TabsTrigger>
        </TabsList>
      </Tabs>
    </header>
  );
}

function shortId(id: string): string {
  return id.length > 8 ? id.slice(0, 8) : id;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function useNavSubscriptions(
  handleBackendStatus: (status: BackendStatus) => void,
  handleSessionEvent: (event: SessionEvent) => void,
  onError: (message: string) => void,
) {
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
