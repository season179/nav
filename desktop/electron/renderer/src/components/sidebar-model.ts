import type { Project, SessionListEntry } from "../lib/project-list.ts";

export const PROJECT_SESSION_PREVIEW_LIMIT = 5;
export const PROJECT_SESSION_DISPLAY_LIMIT = 20;

export type ProjectToggleView = {
  ariaExpanded: "true" | "false";
  ariaLabel: string;
  disclosure: string;
};

export type ProjectSessionToggle = {
  label: string;
  ariaLabel: string;
};

export type ProjectSessionView = {
  visibleSessions: SessionListEntry[];
  toggle: ProjectSessionToggle | null;
};

export function projectToggleView(
  project: Project,
  collapsed: boolean,
): ProjectToggleView {
  const label = projectLabel(project);
  return {
    ariaExpanded: collapsed ? "false" : "true",
    ariaLabel: `${collapsed ? "Expand" : "Collapse"} project ${label}`,
    disclosure: collapsed ? ">" : "v",
  };
}

export function projectSessionView(
  project: Project,
  expanded: boolean,
): ProjectSessionView {
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

export function projectSessionToggle(
  project: Project,
  expanded: boolean,
): ProjectSessionToggle | null {
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
      ariaLabel: `Show fewer sessions in ${projectLabel(project)}`,
    };
  }

  const hiddenCount = displayCount - PROJECT_SESSION_PREVIEW_LIMIT;
  const sessionLabel = hiddenCount === 1 ? "session" : "sessions";
  return {
    label: "Show more",
    ariaLabel: `Show ${hiddenCount} more ${sessionLabel} in ${projectLabel(project)}`,
  };
}

export function projectLabel(project: Project): string {
  return project.displayName ?? project.name;
}
