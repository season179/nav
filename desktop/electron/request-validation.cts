// Pure validation for renderer-originated requests. Keeping it separate from
// preload (which needs Electron) lets the boundary checks be unit tested.

export type ThinkingLevel =
  | "off"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh";

export type SessionMode = "local" | "worktree";

const THINKING_LEVELS = new Set<ThinkingLevel>([
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
]);
const THINKING_LEVEL_ERROR =
  "thinking level must be off, minimal, low, medium, high, or xhigh";

export function normalizeMessageText(value: unknown): string {
  return normalizeRequiredString(value, "message text");
}

export function normalizeSessionId(value: unknown): string {
  return normalizeRequiredString(value, "session id");
}

export function normalizeModelProvider(value: unknown): string {
  return normalizeRequiredString(value, "model provider");
}

export function normalizeModelId(value: unknown): string {
  return normalizeRequiredString(value, "model id");
}

export function normalizeThinkingLevel(value: unknown): ThinkingLevel {
  const level = normalizeRequiredString(value, "thinking level");
  if (!THINKING_LEVELS.has(level as ThinkingLevel)) {
    throw new Error(THINKING_LEVEL_ERROR);
  }
  return level as ThinkingLevel;
}

export function normalizeOptionalThinkingLevel(
  value: unknown,
): ThinkingLevel | null {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeThinkingLevel(value);
}

export function normalizeOptionalWorkspaceRoot(value: unknown): string | null {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeRequiredString(value, "workspace root");
}

export function normalizeOptionalSessionMode(
  value: unknown,
): SessionMode | null {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeSessionMode(value);
}

export function normalizeSessionMode(value: unknown): SessionMode {
  const mode = normalizeRequiredString(value, "session mode");
  if (mode !== "local" && mode !== "worktree") {
    throw new Error("session mode must be local or worktree");
  }
  return mode;
}

function normalizeRequiredString(value: unknown, label: string): string {
  if (typeof value !== "string") {
    throw new TypeError(`${label} must be a string`);
  }
  const text = value.trim();
  if (text.length === 0) {
    throw new Error(`${label} must not be empty`);
  }
  return text;
}
