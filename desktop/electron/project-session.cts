import type { SessionMode } from "./request-validation.cjs";
import type { BackendSession } from "./types.cjs";

export function existingProjectSessionId(
  sessions: BackendSession[] | null | undefined,
  workspaceRoot: string,
  mode: SessionMode = "local",
): string | null {
  const targetRoot = normalizeWorkspaceRoot(workspaceRoot);
  const isWorktree = mode === "worktree";
  if (!targetRoot || !Array.isArray(sessions)) {
    return null;
  }

  let selectedSessionId: string | null = null;
  let selectedActivity = Number.NEGATIVE_INFINITY;

  for (const session of sessions) {
    const sessionId = session?.sessionId;
    if (typeof sessionId !== "string") {
      continue;
    }

    const workspace = normalizeWorkspaceRoot(session?.workspaceRoot);
    const matches = isWorktree
      ? workspace !== "" &&
        workspace !== targetRoot &&
        normalizeWorkspaceRoot(session?.projectRoot) === targetRoot
      : workspace === targetRoot;
    if (!matches) {
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

export function normalizeWorkspaceRoot(value: unknown): string {
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

function sessionActivity(session: BackendSession): number {
  const updatedAt = Number(session?.updatedAt);
  return Number.isFinite(updatedAt) ? updatedAt : 0;
}
