import { useEffect, useMemo, useState } from "react";
import { groupSessionsByProject } from "../lib/project-list.mjs";
import { projectSessionView, projectToggleView } from "./sidebar-model.mjs";

const projectOrderStorageKey = "nav.projectOrder.v1";

export default function Sidebar({
  activeSessionId,
  connected,
  running,
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
          disabled={running || !connected}
          onClick={onNewChat}
        >
          + New chat
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
            disabled={running || !connected}
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
                collapsed={collapsedProjectKeys.has(project.key)}
                connected={connected}
                expanded={expandedProjectSessionKeys.has(project.key)}
                project={project}
                running={running}
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
  collapsed,
  connected,
  expanded,
  project,
  running,
  onNewChatInProject,
  onSelectSession,
  onToggleProject,
  onToggleProjectSessions,
}) {
  const { visibleSessions, toggle } = projectSessionView(project, expanded);
  const toggleView = projectToggleView(project, collapsed);

  return (
    <li className="project-group">
      <ProjectHeading
        connected={connected}
        project={project}
        running={running}
        toggleView={toggleView}
        onNewChatInProject={onNewChatInProject}
        onToggleProject={onToggleProject}
      />
      {collapsed ? null : (
        <ProjectSessions
          activeSessionId={activeSessionId}
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
  toggleView,
  onNewChatInProject,
  onToggleProject,
}) {
  return (
    <div className="project-heading">
      <button
        type="button"
        className="project-toggle"
        title={project.path || project.name}
        aria-label={toggleView.ariaLabel}
        aria-expanded={toggleView.ariaExpanded}
        onClick={onToggleProject}
      >
        <span className="project-label">
          <span className="project-disclosure" aria-hidden="true">
            {toggleView.disclosure}
          </span>
          <span className="project-icon" aria-hidden="true" />
          <span className="project-name">{project.name}</span>
        </span>
      </button>
      <button
        type="button"
        className="project-chat-add"
        title={`New chat in ${project.name}`}
        aria-label={`New chat in ${project.name}`}
        disabled={running || !connected}
        onClick={() => onNewChatInProject(project.path)}
      >
        +
      </button>
    </div>
  );
}

function ProjectSessions({
  activeSessionId,
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
            {sessionTitle(session)}
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
  return title.length > 0 ? title : "New chat";
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
