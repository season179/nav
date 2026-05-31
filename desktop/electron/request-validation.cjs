// Pure validation for renderer-originated requests. Keeping it separate from
// preload (which needs Electron) lets the boundary checks be unit tested.

function normalizeMessageText(value) {
  return normalizeRequiredString(value, "message text");
}

function normalizeSessionId(value) {
  return normalizeRequiredString(value, "session id");
}

function normalizeOptionalWorkspaceRoot(value) {
  if (value === undefined || value === null) {
    return null;
  }
  return normalizeRequiredString(value, "workspace root");
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
  normalizeMessageText,
  normalizeOptionalWorkspaceRoot,
  normalizeSessionId,
};
