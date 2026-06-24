import { queryOptions } from "@tanstack/react-query";
import type {
  ModelInfo,
  ModelOption,
  NavApi,
  SessionStacksResult,
  SessionSummary,
  StackAvailabilityResult,
} from "../types.ts";

export const EMPTY_STACKS_RESULT: SessionStacksResult = { stacks: [] };

export const navQueryKeys = {
  all: ["nav"] as const,
  sessions: () => [...navQueryKeys.all, "sessions"] as const,
  modelOptions: () => [...navQueryKeys.all, "models"] as const,
  modelInfo: (sessionId: string | null | undefined) =>
    [...navQueryKeys.all, "model-info", sessionId ?? "none"] as const,
  stackAvailability: (sessionId: string | null | undefined) =>
    [...navQueryKeys.all, "stack-availability", sessionId ?? "none"] as const,
  stacks: (sessionId: string | null | undefined) =>
    [...navQueryKeys.all, "stacks", sessionId ?? "none"] as const,
};

export function currentNavApi(): NavApi {
  const nav = typeof window === "undefined" ? undefined : window.nav;
  if (!nav) {
    throw new Error("Electron preload API unavailable");
  }
  return nav;
}

export async function fetchNavSessions(
  nav: NavApi = currentNavApi(),
): Promise<SessionSummary[]> {
  return nav.listSessions();
}

export async function fetchModelOptions(
  nav: NavApi = currentNavApi(),
): Promise<ModelOption[]> {
  return nav.modelList();
}

export async function fetchModelInfo(
  sessionId: string,
  nav: NavApi = currentNavApi(),
): Promise<ModelInfo> {
  return nav.modelInfo(sessionId);
}

export async function fetchStackAvailability(
  sessionId: string,
  nav: NavApi = currentNavApi(),
): Promise<StackAvailabilityResult> {
  return nav.sessionStackAvailability(sessionId);
}

export async function fetchSessionStacks(
  sessionId: string,
  nav: NavApi = currentNavApi(),
): Promise<SessionStacksResult> {
  return nav.sessionStacks(sessionId);
}

export const navSessionsQueryOptions = () =>
  queryOptions({
    queryKey: navQueryKeys.sessions(),
    queryFn: () => fetchNavSessions(),
  });

export const modelOptionsQueryOptions = () =>
  queryOptions({
    queryKey: navQueryKeys.modelOptions(),
    queryFn: () => fetchModelOptions(),
  });

export const sessionStacksQueryOptions = (sessionId: string | null) =>
  queryOptions({
    queryKey: navQueryKeys.stacks(sessionId),
    queryFn: () =>
      sessionId ? fetchSessionStacks(sessionId) : EMPTY_STACKS_RESULT,
    enabled: Boolean(sessionId),
  });
