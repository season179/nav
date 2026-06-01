export const noProjectKey = "__no_project__";

export function groupSessionsByProject(sessions, projectOrder = []) {
  const projects = new Map();

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

  const orderIndex = new Map();
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

function addProjectPathHints(projects) {
  const projectsByName = new Map();

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

function uniquePathSuffix(parts, peerParts, ownIndex) {
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

function sortSessionsByActivity(sessions) {
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

function sessionActivity(session) {
  const updatedAt = Number(session.updatedAt);
  return Number.isFinite(updatedAt) ? updatedAt : 0;
}

export function normalizeProjectPath(path) {
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

export function projectKey(path) {
  return path || noProjectKey;
}

export function projectName(path) {
  if (path === "/") {
    return "/";
  }
  const parts = path.split("/").filter(Boolean);
  return parts.at(-1) ?? path;
}

function projectPathParts(path) {
  if (path === "/") {
    return ["/"];
  }
  return path.split("/").filter(Boolean);
}
