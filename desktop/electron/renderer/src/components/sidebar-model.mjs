export const PROJECT_SESSION_PREVIEW_LIMIT = 5;
export const PROJECT_SESSION_DISPLAY_LIMIT = 20;

export function projectToggleView(project, collapsed) {
  return {
    ariaExpanded: String(!collapsed),
    ariaLabel: `${collapsed ? "Expand" : "Collapse"} project ${project.name}`,
    disclosure: collapsed ? ">" : "v",
  };
}

export function projectSessionView(project, expanded) {
  const displaySessions = project.sessions.slice(
    0,
    PROJECT_SESSION_DISPLAY_LIMIT,
  );
  const visibleSessions = expanded
    ? displaySessions
    : displaySessions.slice(0, PROJECT_SESSION_PREVIEW_LIMIT);

  return {
    visibleSessions,
    toggle: projectSessionToggle(project, expanded),
  };
}

export function projectSessionToggle(project, expanded) {
  const displayCount = Math.min(
    project.sessions.length,
    PROJECT_SESSION_DISPLAY_LIMIT,
  );
  if (displayCount <= PROJECT_SESSION_PREVIEW_LIMIT) {
    return null;
  }

  if (expanded) {
    return {
      label: "Show less",
      ariaLabel: `Show fewer sessions in ${project.name}`,
    };
  }

  const hiddenCount = displayCount - PROJECT_SESSION_PREVIEW_LIMIT;
  const sessionLabel = hiddenCount === 1 ? "session" : "sessions";
  return {
    label: "Show more",
    ariaLabel: `Show ${hiddenCount} more ${sessionLabel} in ${project.name}`,
  };
}
