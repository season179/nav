// Pure validation for renderer-originated requests. Keeping it separate from
// preload (which needs Electron) lets the boundary checks be unit tested.

const THINKING_LEVELS = new Set([
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
]);
const THINKING_LEVEL_ERROR =
  "thinking level must be off, minimal, low, medium, high, or xhigh";

function normalizeMessageText(value) {
  return normalizeRequiredString(value, "message text");
}

function normalizeSessionId(value) {
  return normalizeRequiredString(value, "session id");
}

function normalizeModelProvider(value) {
  return normalizeRequiredString(value, "model provider");
}

function normalizeModelId(value) {
  return normalizeRequiredString(value, "model id");
}

function normalizeThinkingLevel(value) {
  const level = normalizeRequiredString(value, "thinking level");
  if (!THINKING_LEVELS.has(level)) {
    throw new Error(THINKING_LEVEL_ERROR);
  }
  return level;
}

function normalizeOptionalThinkingLevel(value) {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeThinkingLevel(value);
}

function normalizeOptionalWorkspaceRoot(value) {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeRequiredString(value, "workspace root");
}

function normalizeOptionalSessionMode(value) {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeSessionMode(value);
}

function normalizeSessionMode(value) {
  const mode = normalizeRequiredString(value, "session mode");
  if (mode !== "local" && mode !== "worktree") {
    throw new Error("session mode must be local or worktree");
  }
  return mode;
}

function normalizeRequiredString(value, label) {
  if (typeof value !== "string") {
    throw new TypeError(`${label} must be a string`);
  }
  const text = value.trim();
  if (text.length === 0) {
    throw new Error(`${label} must not be empty`);
  }
  return text;
}

module.exports = {
  normalizeModelId,
  normalizeMessageText,
  normalizeModelProvider,
  normalizeOptionalSessionMode,
  normalizeOptionalThinkingLevel,
  normalizeOptionalWorkspaceRoot,
  normalizeSessionId,
  normalizeSessionMode,
  normalizeThinkingLevel,
};
