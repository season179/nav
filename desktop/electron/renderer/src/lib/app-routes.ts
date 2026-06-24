export type NavAppView = "chat" | "stacks" | "settings";

export type NavRouteState = {
  canonicalPath: string;
  known: boolean;
  sessionId: string | null;
  view: NavAppView;
};

export const chatPath = () => "/chat";

export const sessionChatPath = (sessionId: string) =>
  `/sessions/${encodeURIComponent(sessionId)}`;

export const sessionStacksPath = (sessionId: string) =>
  `${sessionChatPath(sessionId)}/stacks`;

export const settingsPath = () => "/settings";

export function navPathFor(
  view: NavAppView,
  sessionId: string | null | undefined,
) {
  if (view === "settings") {
    return settingsPath();
  }
  if (!sessionId) {
    return chatPath();
  }
  return view === "stacks"
    ? sessionStacksPath(sessionId)
    : sessionChatPath(sessionId);
}

export function parseNavPathname(pathname: string): NavRouteState {
  const normalized = normalizePathname(pathname);
  if (normalized === "/" || normalized === chatPath()) {
    return routeState("chat", null, chatPath(), normalized !== "/");
  }
  if (normalized === settingsPath()) {
    return routeState("settings", null, settingsPath(), true);
  }

  const sessionMatch = normalized.match(/^\/sessions\/([^/]+)(?:\/(stacks))?$/);
  if (sessionMatch) {
    const sessionId = safeDecodeURIComponent(sessionMatch[1]);
    const view = sessionMatch[2] === "stacks" ? "stacks" : "chat";
    return routeState(view, sessionId, navPathFor(view, sessionId), true);
  }

  return routeState("chat", null, chatPath(), false);
}

function normalizePathname(pathname: string) {
  const withoutTrailingSlash = pathname.replace(/\/+$/, "");
  return withoutTrailingSlash === "" ? "/" : withoutTrailingSlash;
}

function routeState(
  view: NavAppView,
  sessionId: string | null,
  canonicalPath: string,
  known: boolean,
): NavRouteState {
  return { canonicalPath, known, sessionId, view };
}

function safeDecodeURIComponent(value: string) {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}
