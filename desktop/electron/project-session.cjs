function existingProjectSessionId(sessions, workspaceRoot) {
  const targetRoot = normalizeWorkspaceRoot(workspaceRoot);
  if (!targetRoot || !Array.isArray(sessions)) {
    return null;
  }

  let selectedSessionId = null;
  let selectedActivity = Number.NEGATIVE_INFINITY;

  for (const session of sessions) {
    const sessionId = session?.sessionId;
    if (
      typeof sessionId !== "string" ||
      normalizeWorkspaceRoot(session?.workspaceRoot) !== targetRoot
    ) {
      continue;
    }

    const activity = sessionActivity(session);
    if (!selectedSessionId || activity > selectedActivity) {
      selectedSessionId = sessionId;
      selectedActivity = activity;
    }
  }

  return selectedSessionId;
}

function normalizeWorkspaceRoot(value) {
  if (typeof value !== "string") {
    return "";
  }
  const trimmed = value.trim().replaceAll("\\", "/");
  if (trimmed.length === 0) {
    return "";
  }
  const normalized = trimmed.replace(/\/+$/, "");
  return normalized.length > 0 ? normalized : "/";
}

function sessionActivity(session) {
  const updatedAt = Number(session?.updatedAt);
  return Number.isFinite(updatedAt) ? updatedAt : 0;
}

module.exports = {
  existingProjectSessionId,
  normalizeWorkspaceRoot,
};
