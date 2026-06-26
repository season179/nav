import { useVirtualizer } from "@tanstack/react-virtual";
import {
  ChevronRightIcon,
  CircleAlertIcon,
  FolderIcon,
  MessageSquarePlusIcon,
  PlusIcon,
  SparklesIcon,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { Project, SessionListEntry } from "../lib/project-list.ts";
import { groupSessionsByProject } from "../lib/project-list.ts";
import {
  type ProjectSessionToggle,
  type ProjectToggleView,
  projectLabel,
  projectSessionView,
  projectToggleView,
  shouldVirtualizeProjectSessions,
} from "./sidebar-model.ts";

const projectOrderStorageKey = "nav.projectOrder.v1";

export default function Sidebar({
  activeSessionId,
  attentionSessionIds,
  connected,
  runningSessionIds,
  sessions,
  onCreateProject,
  onNewChat,
  onNewChatInProject,
  onSelectSession,
}: {
  activeSessionId: string | null;
  attentionSessionIds: Set<string>;
  connected: boolean;
  runningSessionIds: Set<string>;
  sessions: SessionListEntry[];
  onCreateProject: () => void;
  onNewChat: () => void;
  onNewChatInProject: (projectPath: string) => void;
  onSelectSession: (sessionId: string) => void;
}) {
  const [collapsedProjectKeys, setCollapsedProjectKeys] = useState(
    () => new Set<string>(),
  );
  const [expandedProjectSessionKeys, setExpandedProjectSessionKeys] = useState(
    () => new Set<string>(),
  );
  const [projectOrder, setProjectOrder] = useState(readProjectOrder);
  const projects = useMemo(
    () => groupSessionsByProject(sessions, projectOrder),
    [projectOrder, sessions],
  );

  useEffect(() => {
    const nextOrder = projects.map((project) => project.key);
    if (nextOrder.length === 0) {
      return;
    }
    if (sameOrder(projectOrder, nextOrder)) {
      return;
    }
    setProjectOrder(nextOrder);
    saveProjectOrder(nextOrder);
  }, [projectOrder, projects]);

  function toggleProject(projectKey: string) {
    setCollapsedProjectKeys(toggleSetEntry(projectKey));
  }

  function toggleProjectSessions(projectKey: string) {
    setExpandedProjectSessionKeys(toggleSetEntry(projectKey));
  }

  return (
    <aside
      className="flex h-screen min-h-0 w-80 flex-col border-sidebar-border border-r bg-sidebar text-sidebar-foreground"
      aria-label="Sidebar"
    >
      <div className="border-sidebar-border border-b p-3">
        <Button
          type="button"
          id="new-chat"
          className="h-9 w-full justify-start bg-sidebar-primary text-sidebar-primary-foreground shadow-none hover:bg-sidebar-primary/90"
          disabled={!connected}
          onClick={onNewChat}
        >
          <MessageSquarePlusIcon />
          New thread
        </Button>
      </div>

      <nav
        className="flex min-h-0 flex-1 flex-col gap-2 p-3"
        aria-label="Projects"
      >
        <div className="flex items-center justify-between px-1">
          <h2 className="font-medium text-sidebar-foreground/70 text-xs uppercase tracking-[0.12em]">
            Projects
          </h2>
          <Button
            type="button"
            id="new-project"
            className="text-sidebar-foreground/70 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
            variant="ghost"
            size="icon-xs"
            aria-label="Add project"
            title="Add project"
            disabled={!connected}
            onClick={onCreateProject}
          >
            <PlusIcon />
          </Button>
        </div>
        <ul
          className="min-h-0 flex-1 space-y-3 overflow-y-auto pr-1"
          id="session-list"
        >
          {projects.length === 0 ? (
            <li className="rounded-md border border-sidebar-border/60 border-dashed px-3 py-6 text-center text-sidebar-foreground/55 text-sm">
              No sessions yet
            </li>
          ) : (
            projects.map((project) => (
              <ProjectGroup
                key={project.key}
                activeSessionId={activeSessionId}
                attentionSessionIds={attentionSessionIds}
                collapsed={collapsedProjectKeys.has(project.key)}
                connected={connected}
                expanded={expandedProjectSessionKeys.has(project.key)}
                project={project}
                runningSessionIds={runningSessionIds}
                onNewChatInProject={onNewChatInProject}
                onSelectSession={onSelectSession}
                onToggleProject={() => toggleProject(project.key)}
                onToggleProjectSessions={() =>
                  toggleProjectSessions(project.key)
                }
              />
            ))
          )}
        </ul>
      </nav>

      <div className="h-3 shrink-0" />
    </aside>
  );
}

function ProjectGroup({
  activeSessionId,
  attentionSessionIds,
  collapsed,
  connected,
  expanded,
  project,
  runningSessionIds,
  onNewChatInProject,
  onSelectSession,
  onToggleProject,
  onToggleProjectSessions,
}: {
  activeSessionId: string | null;
  attentionSessionIds: Set<string>;
  collapsed: boolean;
  connected: boolean;
  expanded: boolean;
  project: Project;
  runningSessionIds: Set<string>;
  onNewChatInProject: (projectPath: string) => void;
  onSelectSession: (sessionId: string) => void;
  onToggleProject: () => void;
  onToggleProjectSessions: () => void;
}) {
  const { visibleSessions, toggle } = projectSessionView(project, expanded);
  const toggleView = projectToggleView(project, collapsed);
  // Surface a run hidden inside a collapsed project so the user can tell a
  // background session is still working without expanding it.
  const projectRunning = project.sessions.some((session) =>
    runningSessionIds.has(session.sessionId),
  );
  const projectNeedsAttention = project.sessions.some((session) =>
    attentionSessionIds.has(session.sessionId),
  );

  return (
    <li className="space-y-1">
      <ProjectHeading
        connected={connected}
        project={project}
        running={projectRunning}
        needsAttention={projectNeedsAttention}
        toggleView={toggleView}
        onNewChatInProject={onNewChatInProject}
        onToggleProject={onToggleProject}
      />
      {collapsed ? null : (
        <ProjectSessions
          activeSessionId={activeSessionId}
          attentionSessionIds={attentionSessionIds}
          runningSessionIds={runningSessionIds}
          sessions={visibleSessions}
          toggle={toggle}
          onSelectSession={onSelectSession}
          onToggleProjectSessions={onToggleProjectSessions}
        />
      )}
    </li>
  );
}

function ProjectHeading({
  connected,
  project,
  running,
  needsAttention,
  toggleView,
  onNewChatInProject,
  onToggleProject,
}: {
  connected: boolean;
  project: Project;
  running: boolean;
  needsAttention: boolean;
  toggleView: ProjectToggleView;
  onNewChatInProject: (projectPath: string) => void;
  onToggleProject: () => void;
}) {
  const label = projectLabel(project);

  return (
    <div className="flex items-center gap-1">
      <button
        type="button"
        className="group flex min-w-0 flex-1 items-center justify-between gap-2 rounded-md px-2 py-2 text-left text-sidebar-foreground/90 text-sm outline-none transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring"
        title={project.path || label}
        aria-label={toggleView.ariaLabel}
        aria-expanded={toggleView.ariaExpanded}
        onClick={onToggleProject}
      >
        <span className="flex min-w-0 items-center gap-2">
          <ChevronRightIcon
            className={cn(
              "size-3.5 shrink-0 text-sidebar-foreground/55 transition-transform",
              toggleView.ariaExpanded ? "rotate-90" : "",
            )}
            aria-hidden="true"
          />
          <span className="flex size-6 shrink-0 items-center justify-center rounded-md bg-sidebar-accent text-sidebar-accent-foreground">
            <FolderIcon className="size-3.5" aria-hidden="true" />
          </span>
          <span className="min-w-0">
            <span className="block truncate font-medium">{project.name}</span>
            {project.pathHint ? (
              <span className="block truncate text-sidebar-foreground/50 text-xs">
                {project.pathHint}
              </span>
            ) : null}
          </span>
          {running ? (
            <SparklesIcon
              className="size-3.5 shrink-0 text-sidebar-primary"
              aria-label="Running"
            />
          ) : needsAttention ? (
            <CircleAlertIcon
              className="size-3.5 shrink-0 text-destructive"
              aria-label="Needs attention"
            />
          ) : null}
        </span>
      </button>
      <Button
        type="button"
        className="text-sidebar-foreground/60 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
        variant="ghost"
        size="icon-xs"
        title={`New thread in ${label}`}
        aria-label={`New thread in ${label}`}
        disabled={!connected}
        onClick={() => onNewChatInProject(project.path)}
      >
        <PlusIcon />
      </Button>
    </div>
  );
}

function ProjectSessions({
  activeSessionId,
  attentionSessionIds,
  runningSessionIds,
  sessions,
  toggle,
  onSelectSession,
  onToggleProjectSessions,
}: {
  activeSessionId: string | null;
  attentionSessionIds: Set<string>;
  runningSessionIds: Set<string>;
  sessions: SessionListEntry[];
  toggle: ProjectSessionToggle | null;
  onSelectSession: (sessionId: string) => void;
  onToggleProjectSessions: () => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const shouldVirtualize = shouldVirtualizeProjectSessions(sessions.length);
  const rowVirtualizer = useVirtualizer({
    count: sessions.length,
    estimateSize: () => 41,
    getItemKey: (index) => sessions[index]?.sessionId ?? index,
    getScrollElement: () => scrollRef.current,
    overscan: 4,
    useFlushSync: false,
  });

  return (
    <>
      {shouldVirtualize ? (
        <div ref={scrollRef} className="max-h-72 overflow-y-auto">
          <ul
            className="relative ml-9 space-y-1"
            style={{ height: `${rowVirtualizer.getTotalSize()}px` }}
          >
            {rowVirtualizer.getVirtualItems().map((virtualRow) => {
              const session = sessions[virtualRow.index];
              if (!session) {
                return null;
              }

              return (
                <li
                  key={virtualRow.key}
                  ref={rowVirtualizer.measureElement}
                  className="absolute top-0 left-0 w-full"
                  data-index={virtualRow.index}
                  style={{ transform: `translateY(${virtualRow.start}px)` }}
                >
                  <SessionItem
                    activeSessionId={activeSessionId}
                    attentionSessionIds={attentionSessionIds}
                    runningSessionIds={runningSessionIds}
                    session={session}
                    onSelectSession={onSelectSession}
                  />
                </li>
              );
            })}
          </ul>
        </div>
      ) : (
        <ul className="ml-9 space-y-1">
          {sessions.map((session) => (
            <li key={session.sessionId}>
              <SessionItem
                activeSessionId={activeSessionId}
                attentionSessionIds={attentionSessionIds}
                runningSessionIds={runningSessionIds}
                session={session}
                onSelectSession={onSelectSession}
              />
            </li>
          ))}
        </ul>
      )}
      {toggle ? (
        <SessionToggle
          toggle={toggle}
          onToggleProjectSessions={onToggleProjectSessions}
        />
      ) : null}
    </>
  );
}

function SessionItem({
  activeSessionId,
  attentionSessionIds,
  runningSessionIds,
  session,
  onSelectSession,
}: {
  activeSessionId: string | null;
  attentionSessionIds: Set<string>;
  runningSessionIds: Set<string>;
  session: SessionListEntry;
  onSelectSession: (sessionId: string) => void;
}) {
  return (
    <button
      type="button"
      className={cn(
        "flex h-8 w-full min-w-0 items-center justify-between gap-2 rounded-md px-2 text-left text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
        session.sessionId === activeSessionId
          ? "bg-sidebar-accent text-sidebar-accent-foreground"
          : "text-sidebar-foreground/65 hover:bg-sidebar-accent/70 hover:text-sidebar-accent-foreground",
      )}
      data-session-id={session.sessionId}
      aria-current={session.sessionId === activeSessionId ? "true" : undefined}
      onClick={() => onSelectSession(session.sessionId)}
    >
      <span className="truncate">{sessionTitle(session)}</span>
      {runningSessionIds.has(session.sessionId) ? (
        <span
          className="size-2 shrink-0 rounded-full bg-sidebar-primary"
          role="img"
          aria-label="Running"
        />
      ) : attentionSessionIds.has(session.sessionId) ? (
        <span
          className="size-2 shrink-0 rounded-full bg-destructive"
          role="img"
          aria-label="Needs attention"
        />
      ) : null}
    </button>
  );
}

function SessionToggle({
  toggle,
  onToggleProjectSessions,
}: {
  toggle: ProjectSessionToggle;
  onToggleProjectSessions: () => void;
}) {
  return (
    <ul className="ml-9 space-y-1">
      <li>
        <button
          type="button"
          className="h-8 rounded-md px-2 text-left text-sidebar-foreground/55 text-xs transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
          aria-label={toggle.ariaLabel}
          onClick={onToggleProjectSessions}
        >
          {toggle.label}
        </button>
      </li>
    </ul>
  );
}

function sessionTitle(session: SessionListEntry): string {
  const title = (session.title ?? "").trim();
  return title.length > 0 ? title : "New thread";
}

function toggleSetEntry(key: string) {
  return (current: Set<string>) => {
    const next = new Set(current);
    if (next.has(key)) {
      next.delete(key);
    } else {
      next.add(key);
    }
    return next;
  };
}

function readProjectOrder(): string[] {
  try {
    const raw = window.localStorage.getItem(projectOrderStorageKey);
    const parsed = JSON.parse(raw ?? "[]");
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed.filter((key) => typeof key === "string");
  } catch {
    return [];
  }
}

function saveProjectOrder(order: string[]) {
  try {
    window.localStorage.setItem(projectOrderStorageKey, JSON.stringify(order));
  } catch {
    // Sidebar order is a convenience; private storage failures should not break chat.
  }
}

function sameOrder(left: string[], right: string[]): boolean {
  return (
    left.length === right.length &&
    left.every((value, index) => value === right[index])
  );
}
