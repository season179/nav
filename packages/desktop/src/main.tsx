import { FlueProvider, type UIMessage, useFlueAgent } from "@flue/react";
import { createFlueClient } from "@flue/sdk";
import {
  CircleAlertIcon,
  LoaderCircleIcon,
  MessageSquareTextIcon,
} from "lucide-react";
import { StrictMode, useCallback, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";

import {
  Conversation,
  ConversationContent,
  ConversationScrollButton,
} from "@/components/ai-elements/conversation";
import { Message, MessageContent } from "@/components/ai-elements/message";
import {
  PromptInput,
  PromptInputBody,
  PromptInputFooter,
  PromptInputSubmit,
  PromptInputTextarea,
  PromptInputTools,
} from "@/components/ai-elements/prompt-input";
import { Shimmer } from "@/components/ai-elements/shimmer";
import { AppSidebar } from "@/components/app-sidebar";
import {
  ChatMessageParts,
  hasRenderableMessageParts,
} from "@/components/chat-message-parts";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { TooltipProvider } from "@/components/ui/tooltip";
import type { FlueConnection, FlueServerStatus } from "@/lib/flue-connection";
import { createProjectsClient, type NavProject } from "@/lib/projects-client";
import { createSessionsClient, type NavSession } from "@/lib/sessions-client";

import "./styles.css";

const formatUuidBytes = (bytes: Uint8Array) =>
  [...bytes]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("")
    .replace(/^(.{8})(.{4})(.{4})(.{4})(.{12})$/, "$1-$2-$3-$4-$5");

const createUuidV7 = () => {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);

  const timestamp = BigInt(Date.now());
  bytes[0] = Number((timestamp >> 40n) & 0xffn);
  bytes[1] = Number((timestamp >> 32n) & 0xffn);
  bytes[2] = Number((timestamp >> 24n) & 0xffn);
  bytes[3] = Number((timestamp >> 16n) & 0xffn);
  bytes[4] = Number((timestamp >> 8n) & 0xffn);
  bytes[5] = Number(timestamp & 0xffn);
  bytes[6] = (bytes[6] & 0x0f) | 0x70;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;

  return formatUuidBytes(bytes);
};

const ACTIVE_PROJECT_STORAGE_KEY = "nav.activeProjectId";
const ACTIVE_SESSION_BY_PROJECT_STORAGE_KEY = "nav.activeSessionIdByProject";
const LEGACY_ACTIVE_SESSION_STORAGE_KEY = "nav.activeSessionId";

type ActiveSessionByProject = Record<string, string>;

const rememberActiveProject = (id: string) => {
  window.localStorage.setItem(ACTIVE_PROJECT_STORAGE_KEY, id);
};

const getRememberedActiveProject = () =>
  window.localStorage.getItem(ACTIVE_PROJECT_STORAGE_KEY);

const getLegacyRememberedActiveSession = () =>
  window.localStorage.getItem(LEGACY_ACTIVE_SESSION_STORAGE_KEY);

const rememberActiveSessionByProject = (value: ActiveSessionByProject) => {
  window.localStorage.setItem(
    ACTIVE_SESSION_BY_PROJECT_STORAGE_KEY,
    JSON.stringify(value),
  );
};

const getRememberedActiveSessionByProject = (): ActiveSessionByProject => {
  try {
    const parsed: unknown = JSON.parse(
      window.localStorage.getItem(ACTIVE_SESSION_BY_PROJECT_STORAGE_KEY) ??
        "{}",
    );

    if (
      typeof parsed !== "object" ||
      parsed === null ||
      Array.isArray(parsed)
    ) {
      return {};
    }

    return Object.fromEntries(
      Object.entries(parsed).filter(
        (entry): entry is [string, string] =>
          typeof entry[0] === "string" && typeof entry[1] === "string",
      ),
    );
  } catch {
    return {};
  }
};

function EmptyConversation() {
  return (
    <Empty className="min-h-0 border-0 px-6 py-10">
      <EmptyHeader>
        <EmptyMedia className="size-10 rounded-xl" variant="icon">
          <MessageSquareTextIcon aria-hidden="true" className="size-5" />
        </EmptyMedia>
        <EmptyTitle>Message Nav</EmptyTitle>
        <EmptyDescription>
          Start a conversation with the local Nav agent.
        </EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

function ConnectionEmpty({
  message,
  state,
}: {
  message: string;
  state: "failed" | "starting";
}) {
  const Icon = state === "failed" ? CircleAlertIcon : LoaderCircleIcon;

  return (
    <Empty className="min-h-0 border-0 px-6 py-10">
      <EmptyHeader>
        <EmptyMedia className="size-10 rounded-xl" variant="icon">
          <Icon
            aria-hidden="true"
            className={state === "starting" ? "size-5 animate-spin" : "size-5"}
          />
        </EmptyMedia>
        <EmptyTitle>
          {state === "failed" ? "Nav is unavailable" : "Starting Nav"}
        </EmptyTitle>
        <EmptyDescription>{message}</EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

type MessagePart = UIMessage["parts"][number];
type ConversationRenderItem =
  | {
      isLatestMessage: boolean;
      key: string;
      message: UIMessage;
      type: "message";
    }
  | {
      isLatestMessage: boolean;
      key: string;
      messages: UIMessage[];
      type: "assistant-group";
    };

const hasVisiblePart = (part: MessagePart) => {
  switch (part.type) {
    case "text":
    case "reasoning":
      return part.text.trim().length > 0;
    case "dynamic-tool":
      return true;
    default:
      return false;
  }
};

const hasVisibleAssistantOutputAfterLastUser = (messages: UIMessage[]) => {
  const lastUserIndex = messages.findLastIndex(
    (message) => message.role === "user",
  );

  return messages
    .slice(lastUserIndex + 1)
    .some(
      (message) =>
        message.role === "assistant" && message.parts.some(hasVisiblePart),
    );
};

const createConversationRenderItems = (messages: UIMessage[]) => {
  const items: ConversationRenderItem[] = [];
  let pendingAssistantGroup: UIMessage[] = [];

  const flushAssistantGroup = () => {
    if (pendingAssistantGroup.length === 0) {
      return;
    }

    items.push({
      isLatestMessage:
        pendingAssistantGroup.at(-1) === messages[messages.length - 1],
      key: pendingAssistantGroup[0]?.id ?? "assistant-group",
      messages: pendingAssistantGroup,
      type: "assistant-group",
    });
    pendingAssistantGroup = [];
  };

  for (const message of messages) {
    if (message.role === "assistant") {
      pendingAssistantGroup.push(message);
      continue;
    }

    flushAssistantGroup();
    items.push({
      isLatestMessage: message === messages[messages.length - 1],
      key: message.id,
      message,
      type: "message",
    });
  }

  flushAssistantGroup();

  return items;
};

function AssistantMessageGroup({
  isLatestMessage,
  messages,
}: {
  isLatestMessage: boolean;
  messages: UIMessage[];
}) {
  const parts = messages.flatMap((message) => message.parts);

  if (!hasRenderableMessageParts(parts)) {
    return null;
  }

  const lastMessageWithMetadata = messages.findLast(
    (message) => message.metadata,
  );
  const message: UIMessage = {
    id: messages[0]?.id ?? "assistant-group",
    metadata: lastMessageWithMetadata?.metadata,
    parts,
    role: "assistant",
  };

  return (
    <Message from="assistant">
      <MessageContent>
        <ChatMessageParts isLatestMessage={isLatestMessage} message={message} />
      </MessageContent>
    </Message>
  );
}

function ConversationMessage({
  isLatestMessage,
  message,
}: {
  isLatestMessage: boolean;
  message: UIMessage;
}) {
  if (!hasRenderableMessageParts(message.parts)) {
    return null;
  }

  return (
    <Message from={message.role === "assistant" ? "assistant" : "user"}>
      <MessageContent>
        <ChatMessageParts isLatestMessage={isLatestMessage} message={message} />
      </MessageContent>
    </Message>
  );
}

function ThinkingMessage() {
  return (
    <Message from="assistant">
      <MessageContent>
        <Shimmer duration={1}>Thinking...</Shimmer>
      </MessageContent>
    </Message>
  );
}

function LiveConversation({
  isThinking,
  messages,
}: {
  isThinking: boolean;
  messages: UIMessage[];
}) {
  if (messages.length === 0 && !isThinking) {
    return <EmptyConversation />;
  }

  const renderItems = createConversationRenderItems(messages);

  return (
    <Conversation className="min-h-0">
      <ConversationContent className="mx-auto w-full max-w-3xl px-6 pt-14 pb-8">
        {renderItems.map((item) =>
          item.type === "assistant-group" ? (
            <AssistantMessageGroup
              isLatestMessage={item.isLatestMessage}
              key={item.key}
              messages={item.messages}
            />
          ) : (
            <ConversationMessage
              isLatestMessage={item.isLatestMessage}
              key={item.key}
              message={item.message}
            />
          ),
        )}
        {isThinking && <ThinkingMessage />}
      </ConversationContent>
      <ConversationScrollButton aria-label="Scroll to bottom" />
    </Conversation>
  );
}

function PromptComposer({
  disabled,
  onSubmit,
  status,
}: {
  disabled?: boolean;
  onSubmit: (message: string) => Promise<void>;
  status?: "error" | "submitted" | "streaming";
}) {
  return (
    <div className="shrink-0 bg-background/95 px-4 py-3 backdrop-blur">
      <PromptInput
        aria-label="Chat prompt"
        className="mx-auto max-w-3xl"
        onSubmit={async (message) => {
          const text = message.text.trim();

          if (!text || disabled) {
            return;
          }

          await onSubmit(text);
        }}
      >
        <PromptInputBody>
          <PromptInputTextarea disabled={disabled} placeholder="Message Nav" />
        </PromptInputBody>
        <PromptInputFooter>
          <PromptInputTools />
          <PromptInputSubmit disabled={disabled} status={status} />
        </PromptInputFooter>
      </PromptInput>
    </div>
  );
}

function NavChat({
  conversationId,
  isDraft,
  onEnsureSessionBound,
  onSessionAdmitted,
  project,
  serverStatus,
}: {
  conversationId: string;
  isDraft: boolean;
  onEnsureSessionBound: (id: string) => Promise<void>;
  onSessionAdmitted: (id: string, title: string) => Promise<void>;
  project: NavProject | null;
  serverStatus: FlueServerStatus | null;
}) {
  const [sessionError, setSessionError] = useState<string | null>(null);
  const { messages, status, error, sendMessage } = useFlueAgent({
    history: "all",
    id: conversationId,
    name: "nav",
  });

  const serverReady = serverStatus?.state === "ready";
  // Only block the composer while the server is down or a send is in flight.
  // History hydration uses a "connecting" status that retries indefinitely on
  // recoverable backend errors (e.g. a 503 while Codex auth is unavailable);
  // gating on it would leave the composer permanently disabled with no way to
  // type, retry, or surface the error.
  const sending = status === "submitted" || status === "streaming";
  const projectUnavailable = project !== null && !project.available;
  const disabled = !serverReady || sending || projectUnavailable;

  const composerStatus =
    status === "error"
      ? "error"
      : status === "streaming"
        ? "streaming"
        : status === "submitted"
          ? "submitted"
          : undefined;
  const isThinking =
    status === "submitted" ||
    (status === "streaming" &&
      !hasVisibleAssistantOutputAfterLastUser(messages));

  return (
    <div className="flex h-full min-h-0 flex-1 flex-col overflow-hidden">
      {(error || sessionError) && (
        <div className="mx-auto mt-4 w-full max-w-3xl rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-destructive text-sm">
          {error?.message ?? sessionError}
        </div>
      )}
      {projectUnavailable && (
        <div className="mx-auto mt-4 w-full max-w-3xl rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-destructive text-sm">
          Project folder is unavailable: {project.path}
        </div>
      )}
      <LiveConversation isThinking={isThinking} messages={messages} />
      <PromptComposer
        disabled={disabled}
        onSubmit={async (text) => {
          setSessionError(null);
          if (isDraft) {
            try {
              await onEnsureSessionBound(conversationId);
            } catch (caught) {
              setSessionError(
                caught instanceof Error
                  ? caught.message
                  : "Chat could not be attached to this project.",
              );
              return;
            }
          }

          await sendMessage(text);
          try {
            await onSessionAdmitted(conversationId, text);
          } catch (caught) {
            setSessionError(
              caught instanceof Error
                ? caught.message
                : "Chat was saved, but the sidebar did not refresh.",
            );
          }
        }}
        status={composerStatus}
      />
    </div>
  );
}

function ConnectedApp({
  activeSessionId,
  connection,
  isDraftActive,
  onEnsureSessionBound,
  onSessionAdmitted,
  project,
  serverStatus,
}: {
  activeSessionId: string | null;
  connection: FlueConnection;
  isDraftActive: boolean;
  onEnsureSessionBound: (id: string) => Promise<void>;
  onSessionAdmitted: (id: string, title: string) => Promise<void>;
  project: NavProject | null;
  serverStatus: FlueServerStatus | null;
}) {
  const client = useMemo(
    () =>
      createFlueClient({
        baseUrl: connection.baseUrl,
        fetch: window.fetch.bind(window),
        token: connection.token,
      }),
    [connection.baseUrl, connection.token],
  );

  if (!activeSessionId) {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <ConnectionEmpty message="Loading chats." state="starting" />
      </div>
    );
  }

  return (
    <FlueProvider client={client}>
      <NavChat
        conversationId={activeSessionId}
        key={activeSessionId}
        isDraft={isDraftActive}
        onEnsureSessionBound={onEnsureSessionBound}
        onSessionAdmitted={onSessionAdmitted}
        project={project}
        serverStatus={serverStatus}
      />
    </FlueProvider>
  );
}

function AppShell() {
  const [activeProjectId, setActiveProjectId] = useState<string | null>(() =>
    getRememberedActiveProject(),
  );
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [activeSessionIdByProject, setActiveSessionIdByProject] =
    useState<ActiveSessionByProject>(() =>
      getRememberedActiveSessionByProject(),
    );
  const [connection, setConnection] = useState<FlueConnection | null>(null);
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [draftSessionIdByProject, setDraftSessionIdByProject] =
    useState<ActiveSessionByProject>({});
  const [projects, setProjects] = useState<NavProject[]>([]);
  const [projectsError, setProjectsError] = useState<string | null>(null);
  const [projectsLoaded, setProjectsLoaded] = useState(false);
  const [projectsLoading, setProjectsLoading] = useState(false);
  const [sessions, setSessions] = useState<NavSession[]>([]);
  const [sessionsError, setSessionsError] = useState<string | null>(null);
  const [sessionsLoaded, setSessionsLoaded] = useState(false);
  const [sessionsLoading, setSessionsLoading] = useState(false);
  const [serverStatus, setServerStatus] = useState<FlueServerStatus | null>(
    null,
  );
  const sessionsClient = useMemo(
    () => (connection ? createSessionsClient(connection) : null),
    [connection],
  );
  const projectsClient = useMemo(
    () => (connection ? createProjectsClient(connection) : null),
    [connection],
  );

  const activeProject = useMemo(
    () => projects.find((project) => project.id === activeProjectId) ?? null,
    [activeProjectId, projects],
  );
  const isDraftActive =
    activeProjectId !== null &&
    draftSessionIdByProject[activeProjectId] === activeSessionId;

  const activateSession = useCallback(
    (projectId: string, sessionId: string, draft: boolean) => {
      setActiveProjectId(projectId);
      setActiveSessionId(sessionId);
      rememberActiveProject(projectId);
      setActiveSessionIdByProject((current) => {
        const next = { ...current, [projectId]: sessionId };
        rememberActiveSessionByProject(next);
        return next;
      });
      setDraftSessionIdByProject((current) => {
        const next = { ...current };

        if (draft) {
          next[projectId] = sessionId;
        } else {
          delete next[projectId];
        }

        return next;
      });
    },
    [],
  );

  const activateProjectWithSession = useCallback(
    (projectId: string) => {
      const projectSessions = sessions.filter(
        (session) => session.projectId === projectId,
      );
      const rememberedSession = projectSessions.find(
        (session) => session.id === activeSessionIdByProject[projectId],
      );
      const draftSessionId = draftSessionIdByProject[projectId];
      const nextSessionId =
        rememberedSession?.id ??
        draftSessionId ??
        projectSessions[0]?.id ??
        createUuidV7();

      activateSession(
        projectId,
        nextSessionId,
        !rememberedSession &&
          (nextSessionId === draftSessionId || projectSessions.length === 0),
      );
    },
    [
      activateSession,
      activeSessionIdByProject,
      draftSessionIdByProject,
      sessions,
    ],
  );

  const refreshProjects = useCallback(async () => {
    if (!projectsClient) {
      setProjects([]);
      setProjectsLoaded(false);

      return [];
    }

    setProjectsError(null);
    setProjectsLoaded(false);
    setProjectsLoading(true);

    try {
      const nextProjects = await projectsClient.listProjects();
      setProjects(nextProjects);

      return nextProjects;
    } catch (caught) {
      setProjectsError(
        caught instanceof Error ? caught.message : "Unable to load projects.",
      );

      return [];
    } finally {
      setProjectsLoaded(true);
      setProjectsLoading(false);
    }
  }, [projectsClient]);

  const refreshSessions = useCallback(async () => {
    if (!sessionsClient) {
      setSessions([]);
      setSessionsLoaded(false);

      return [];
    }

    setSessionsError(null);
    setSessionsLoaded(false);
    setSessionsLoading(true);

    try {
      const nextSessions = await sessionsClient.listSessions();
      setSessions(nextSessions);

      return nextSessions;
    } catch (caught) {
      setSessionsError(
        caught instanceof Error ? caught.message : "Unable to load chats.",
      );

      return [];
    } finally {
      setSessionsLoaded(true);
      setSessionsLoading(false);
    }
  }, [sessionsClient]);

  const refreshWorkspace = useCallback(async () => {
    await Promise.all([refreshProjects(), refreshSessions()]);
  }, [refreshProjects, refreshSessions]);

  useEffect(() => {
    const unsubscribe = window.navDesktop.onFlueStatus(setServerStatus);

    window.navDesktop
      .getFlueConnection()
      .then((nextConnection) => {
        setConnection(nextConnection);
        setServerStatus(nextConnection.status);
      })
      .catch((error: unknown) => {
        setConnectionError(
          error instanceof Error ? error.message : "Unable to connect to Nav.",
        );
      });

    return unsubscribe;
  }, []);

  useEffect(() => {
    void refreshWorkspace();
  }, [refreshWorkspace]);

  useEffect(() => {
    if (!sessionsClient && !projectsClient) {
      return;
    }

    const handleFocus = () => {
      void refreshWorkspace();
    };

    window.addEventListener("focus", handleFocus);

    return () => window.removeEventListener("focus", handleFocus);
  }, [projectsClient, refreshWorkspace, sessionsClient]);

  useEffect(() => {
    if (
      !projectsLoaded ||
      !sessionsLoaded ||
      !connection ||
      projectsError ||
      sessionsError
    ) {
      return;
    }

    if (projects.length === 0) {
      return;
    }

    const activeProjectExists =
      activeProjectId !== null &&
      projects.some((project) => project.id === activeProjectId);

    if (!activeProjectExists) {
      const rememberedProject = projects.find(
        (project) => project.id === getRememberedActiveProject(),
      );
      const legacySession = sessions.find(
        (session) => session.id === getLegacyRememberedActiveSession(),
      );
      const legacyProjectId = legacySession?.projectId || undefined;
      const defaultProject = projects.find((project) => project.isDefault);
      const nextProjectId =
        rememberedProject?.id ??
        legacyProjectId ??
        defaultProject?.id ??
        projects[0]?.id;

      if (nextProjectId) {
        activateProjectWithSession(nextProjectId);
      }
      return;
    }

    if (!activeSessionId) {
      activateProjectWithSession(activeProjectId);
      return;
    }

    if (draftSessionIdByProject[activeProjectId] === activeSessionId) {
      return;
    }

    const activeSession = sessions.find(
      (session) => session.id === activeSessionId,
    );

    if (activeSession?.projectId === activeProjectId) {
      return;
    }

    activateProjectWithSession(activeProjectId);
  }, [
    activateProjectWithSession,
    activeProjectId,
    activeSessionId,
    connection,
    draftSessionIdByProject,
    projects,
    projectsError,
    projectsLoaded,
    sessions,
    sessionsError,
    sessionsLoaded,
  ]);

  const handleNewChat = useCallback(
    (projectId = activeProjectId) => {
      if (!projectId) {
        return;
      }

      const project = projects.find((candidate) => candidate.id === projectId);

      if (!project?.available) {
        return;
      }

      activateSession(projectId, createUuidV7(), true);
    },
    [activateSession, activeProjectId, projects],
  );

  const handleEnsureSessionBound = useCallback(
    async (id: string) => {
      if (!sessionsClient || !activeProjectId) {
        throw new Error("Project is not ready.");
      }

      await sessionsClient.createSession(id, null, activeProjectId);
    },
    [activeProjectId, sessionsClient],
  );

  const handleSessionAdmitted = useCallback(
    async (id: string, title: string) => {
      if (!sessionsClient || !activeProjectId) {
        return;
      }

      await sessionsClient.createSession(id, title, activeProjectId);

      const nextSessions = await refreshSessions();
      void refreshProjects();

      if (nextSessions.some((session) => session.id === id)) {
        setDraftSessionIdByProject((current) => {
          if (current[activeProjectId] !== id) {
            return current;
          }

          const next = { ...current };
          delete next[activeProjectId];
          return next;
        });
      }
    },
    [activeProjectId, refreshProjects, refreshSessions, sessionsClient],
  );

  const handleRenameSession = useCallback(
    async (id: string, title: string) => {
      if (!sessionsClient) {
        return;
      }

      await sessionsClient.renameSession(id, title);
      await refreshSessions();
    },
    [refreshSessions, sessionsClient],
  );

  const handleDeleteSession = useCallback(
    async (id: string) => {
      if (!sessionsClient) {
        return;
      }

      const deletedSession = sessions.find((session) => session.id === id);
      const projectId = deletedSession?.projectId ?? activeProjectId;

      await sessionsClient.deleteSession(id);

      setDraftSessionIdByProject((current) => {
        const next = Object.fromEntries(
          Object.entries(current).filter((entry) => entry[1] !== id),
        );

        return next;
      });

      if (activeSessionId === id) {
        if (!projectId) {
          setActiveSessionId(null);
        } else {
          const fallback = sessions.find(
            (session) => session.id !== id && session.projectId === projectId,
          );

          activateSession(projectId, fallback?.id ?? createUuidV7(), !fallback);
        }
      }

      await refreshSessions();
    },
    [
      activateSession,
      activeProjectId,
      activeSessionId,
      refreshSessions,
      sessions,
      sessionsClient,
    ],
  );

  const handleAddProject = useCallback(async () => {
    if (!projectsClient) {
      return;
    }

    setProjectsError(null);

    const path = await window.navDesktop.pickProjectDirectory();

    if (!path) {
      return;
    }

    const project = await projectsClient.createProject(path);
    await refreshWorkspace();
    activateSession(project.id, createUuidV7(), true);
  }, [activateSession, projectsClient, refreshWorkspace]);

  const handleRenameProject = useCallback(
    async (id: string, name: string) => {
      if (!projectsClient) {
        return;
      }

      await projectsClient.renameProject(id, name);
      await refreshProjects();
    },
    [projectsClient, refreshProjects],
  );

  const handleRemoveProject = useCallback(
    async (id: string) => {
      if (!projectsClient) {
        return;
      }

      await projectsClient.removeProject(id);
      setActiveSessionIdByProject((current) => {
        const next = { ...current };
        delete next[id];
        rememberActiveSessionByProject(next);
        return next;
      });
      setDraftSessionIdByProject((current) => {
        const next = { ...current };
        delete next[id];
        return next;
      });

      if (activeProjectId === id) {
        setActiveProjectId(null);
        setActiveSessionId(null);
      }

      await refreshWorkspace();
    },
    [activeProjectId, projectsClient, refreshWorkspace],
  );

  const sidebarError = projectsError ?? sessionsError;

  const chatContent = (() => {
    if (connectionError) {
      return <ConnectionEmpty message={connectionError} state="failed" />;
    }

    if (!connection) {
      return (
        <ConnectionEmpty
          message={
            serverStatus?.message ?? "Waiting for the local Flue server."
          }
          state={serverStatus?.state === "failed" ? "failed" : "starting"}
        />
      );
    }

    return (
      <ConnectedApp
        activeSessionId={activeSessionId}
        connection={connection}
        isDraftActive={isDraftActive}
        onEnsureSessionBound={handleEnsureSessionBound}
        onSessionAdmitted={handleSessionAdmitted}
        project={activeProject}
        serverStatus={serverStatus}
      />
    );
  })();

  return (
    <>
      <AppSidebar
        activeProjectId={activeProjectId}
        activeSessionId={activeSessionId}
        error={sidebarError}
        loading={projectsLoading || sessionsLoading}
        onAddProject={handleAddProject}
        onDeleteSession={handleDeleteSession}
        onNewChat={handleNewChat}
        onRemoveProject={handleRemoveProject}
        onRenameProject={handleRenameProject}
        onRenameSession={handleRenameSession}
        onSelectProject={activateProjectWithSession}
        onSelectSession={(session) =>
          activateSession(session.projectId, session.id, false)
        }
        projects={projects}
        sessions={sessions}
      />
      <div className="fixed inset-x-0 top-0 z-40 h-10 [-webkit-app-region:drag]" />
      <SidebarTrigger className="fixed top-1 left-[76px] z-50 [-webkit-app-region:no-drag] [&_svg]:!size-[18px]" />
      <SidebarInset className="h-svh overflow-hidden pt-10">
        <div className="flex h-full min-h-0 flex-1 flex-col">{chatContent}</div>
      </SidebarInset>
    </>
  );
}

function App() {
  return (
    <TooltipProvider>
      <SidebarProvider className="h-svh overflow-hidden">
        <AppShell />
      </SidebarProvider>
    </TooltipProvider>
  );
}

const root = document.createElement("div");
root.id = "root";
document.body.replaceChildren(root);

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
