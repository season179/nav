import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useMemo, useRef, useState } from "react";
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
    <aside className="sidebar" aria-label="Sidebar">
      <div className="sidebar-header">
        <button
          type="button"
          id="new-chat"
          className="new-chat"
          disabled={!connected}
          onClick={onNewChat}
        >
          + New thread
        </button>
      </div>

      <nav className="sidebar-section" aria-label="Projects">
        <div className="sidebar-section-heading">
          <h2 className="sidebar-section-title">Projects</h2>
          <button
            type="button"
            id="new-project"
            className="project-add"
            aria-label="Add project"
            title="Add project"
            disabled={!connected}
            onClick={onCreateProject}
          >
            +
          </button>
        </div>
        <ul className="session-list" id="session-list">
          {projects.length === 0 ? (
            <li className="sidebar-empty">No sessions yet</li>
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

      <div className="sidebar-footer" />
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
    <li className="project-group">
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
    <div className="project-heading">
      <button
        type="button"
        className="project-toggle"
        title={project.path || label}
        aria-label={toggleView.ariaLabel}
        aria-expanded={toggleView.ariaExpanded}
        onClick={onToggleProject}
      >
        <span className="project-label">
          <span className="project-disclosure" aria-hidden="true">
            {toggleView.disclosure}
          </span>
          <span className="project-icon" aria-hidden="true" />
          <span className="project-title">
            <span className="project-name">{project.name}</span>
            {project.pathHint ? (
              <span className="project-path-hint">{project.pathHint}</span>
            ) : null}
          </span>
          {running ? (
            <span
              className="project-running"
              role="img"
              aria-label="Running"
              title="Running"
            />
          ) : needsAttention ? (
            <span
              className="project-attention"
              role="img"
              aria-label="Needs attention"
              title="Needs attention"
            />
          ) : null}
        </span>
      </button>
      <button
        type="button"
        className="project-chat-add"
        title={`New thread in ${label}`}
        aria-label={`New thread in ${label}`}
        disabled={!connected}
        onClick={() => onNewChatInProject(project.path)}
      >
        +
      </button>
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
        <div ref={scrollRef} className="project-session-virtual-viewport">
          <ul
            className="project-session-list project-session-list-virtual"
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
                  className="project-session-virtual-row"
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
        <ul className="project-session-list">
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
      className="session-item"
      data-session-id={session.sessionId}
      aria-current={session.sessionId === activeSessionId ? "true" : undefined}
      onClick={() => onSelectSession(session.sessionId)}
    >
      <span className="session-item-title">{sessionTitle(session)}</span>
      {runningSessionIds.has(session.sessionId) ? (
        <span
          className="session-running"
          role="img"
          aria-label="Running"
          title="Running"
        />
      ) : attentionSessionIds.has(session.sessionId) ? (
        <span
          className="session-attention"
          role="img"
          aria-label="Needs attention"
          title="Needs attention"
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
    <ul className="project-session-list project-session-toggle-list">
      <li>
        <button
          type="button"
          className="project-session-toggle"
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
