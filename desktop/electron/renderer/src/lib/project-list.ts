// One session as seen by the sidebar. Mirrors the backend's session summary but
// keeps every field optional/loose because the grouping logic is fed raw RPC
// results and tolerates partial rows.
export type SessionListEntry = {
  sessionId: string;
  title?: string | null;
  workspaceRoot?: string | null;
  projectRoot?: string | null;
  updatedAt?: number | string | null;
};

// A grouped project rendered in the sidebar.
export type Project = {
  key: string;
  path: string;
  name: string;
  displayName?: string;
  pathHint?: string | null;
  sessions: SessionListEntry[];
};

type SessionEntry = {
  originalIndex: number;
  session: SessionListEntry;
};

type ProjectAccumulator = {
  key: string;
  path: string;
  name: string;
  sessionEntries: SessionEntry[];
  firstSeenIndex: number;
};

export const noProjectKey = "__no_project__";

export function groupSessionsByProject(
  sessions: SessionListEntry[],
  projectOrder: string[] = [],
): Project[] {
  const projects = new Map<string, ProjectAccumulator>();

  for (const [index, session] of sessions.entries()) {
    const path = normalizeProjectPath(
      session.projectRoot ?? session.workspaceRoot,
    );
    const key = projectKey(path);
    let project = projects.get(key);
    if (!project) {
      project = {
        key,
        path,
        name: path ? projectName(path) : "No project",
        sessionEntries: [],
        firstSeenIndex: index,
      };
      projects.set(key, project);
    }
    project.sessionEntries.push({ originalIndex: index, session });
  }

  const orderIndex = new Map<string, number>();
  for (const [index, key] of projectOrder.entries()) {
    if (typeof key === "string" && !orderIndex.has(key)) {
      orderIndex.set(key, index);
    }
  }

  const orderedProjects = Array.from(projects.values()).sort((left, right) => {
    const leftIndex = orderIndex.get(left.key) ?? Number.POSITIVE_INFINITY;
    const rightIndex = orderIndex.get(right.key) ?? Number.POSITIVE_INFINITY;

    if (leftIndex !== rightIndex) {
      return leftIndex - rightIndex;
    }

    return left.firstSeenIndex - right.firstSeenIndex;
  });

  return addProjectPathHints(
    orderedProjects.map((project) => ({
      key: project.key,
      path: project.path,
      name: project.name,
      displayName: project.name,
      pathHint: null,
      sessions: sortSessionsByActivity(project.sessionEntries),
    })),
  );
}

function addProjectPathHints(projects: Project[]): Project[] {
  const projectsByName = new Map<string, Project[]>();

  for (const project of projects) {
    if (!project.path) {
      continue;
    }
    const nameProjects = projectsByName.get(project.name) ?? [];
    nameProjects.push(project);
    projectsByName.set(project.name, nameProjects);
  }

  for (const nameProjects of projectsByName.values()) {
    if (nameProjects.length <= 1) {
      continue;
    }

    const peerParts = nameProjects.map((project) =>
      projectPathParts(project.path),
    );
    for (const [index, project] of nameProjects.entries()) {
      const pathHint = uniquePathSuffix(peerParts[index], peerParts, index);
      project.pathHint = pathHint;
      project.displayName = `${project.name} (${pathHint})`;
    }
  }

  return projects;
}

function uniquePathSuffix(
  parts: string[],
  peerParts: string[][],
  ownIndex: number,
): string {
  for (let length = 2; length <= parts.length; length += 1) {
    const suffix = parts.slice(-length).join("/");
    const isUnique = peerParts.every(
      (peer, index) =>
        index === ownIndex || peer.slice(-length).join("/") !== suffix,
    );
    if (isUnique) {
      return suffix;
    }
  }

  return parts.join("/");
}

function sortSessionsByActivity(sessions: SessionEntry[]): SessionListEntry[] {
  return sessions
    .slice()
    .sort((left, right) => {
      const activityDelta =
        sessionActivity(right.session) - sessionActivity(left.session);
      if (activityDelta !== 0) {
        return activityDelta;
      }
      return left.originalIndex - right.originalIndex;
    })
    .map((entry) => entry.session);
}

function sessionActivity(session: SessionListEntry): number {
  const updatedAt = Number(session.updatedAt);
  return Number.isFinite(updatedAt) ? updatedAt : 0;
}

export function normalizeProjectPath(path: unknown): string {
  if (typeof path !== "string") {
    return "";
  }
  const trimmed = path.trim().replaceAll("\\", "/");
  if (trimmed.length === 0) {
    return "";
  }
  const normalized = trimmed.replace(/\/+$/, "");
  return normalized.length > 0 ? normalized : "/";
}

export function projectKey(path: string): string {
  return path || noProjectKey;
}

export function projectName(path: string): string {
  if (path === "/") {
    return "/";
  }
  const parts = path.split("/").filter(Boolean);
  return parts.at(-1) ?? path;
}

function projectPathParts(path: string): string[] {
  if (path === "/") {
    return ["/"];
  }
  return path.split("/").filter(Boolean);
}
