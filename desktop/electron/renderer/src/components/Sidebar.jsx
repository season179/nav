import { useEffect, useMemo, useState } from "react";
import { groupSessionsByProject } from "../lib/project-list.mjs";
import {
  projectLabel,
  projectSessionView,
  projectToggleView,
} from "./sidebar-model.mjs";

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
}) {
  const [collapsedProjectKeys, setCollapsedProjectKeys] = useState(
    () => new Set(),
  );
  const [expandedProjectSessionKeys, setExpandedProjectSessionKeys] = useState(
    () => new Set(),
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

  function toggleProject(projectKey) {
    setCollapsedProjectKeys(toggleSetEntry(projectKey));
  }

  function toggleProjectSessions(projectKey) {
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
}) {
  return (
    <ul className="project-session-list">
      {sessions.map((session) => (
        <li key={session.sessionId}>
          <button
            type="button"
            className="session-item"
            data-session-id={session.sessionId}
            aria-current={
              session.sessionId === activeSessionId ? "true" : undefined
            }
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
        </li>
      ))}
      {toggle ? (
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
      ) : null}
    </ul>
  );
}

function sessionTitle(session) {
  const title = (session.title ?? "").trim();
  return title.length > 0 ? title : "New thread";
}

function toggleSetEntry(key) {
  return (current) => {
    const next = new Set(current);
    if (next.has(key)) {
      next.delete(key);
    } else {
      next.add(key);
    }
    return next;
  };
}

function readProjectOrder() {
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

function saveProjectOrder(order) {
  try {
    window.localStorage.setItem(projectOrderStorageKey, JSON.stringify(order));
  } catch {
    // Sidebar order is a convenience; private storage failures should not break chat.
  }
}

function sameOrder(left, right) {
  return (
    left.length === right.length &&
    left.every((value, index) => value === right[index])
  );
}
