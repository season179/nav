// Pure validation for renderer-originated requests. Keeping it separate from
// preload (which needs Electron) lets the boundary checks be unit tested.

function normalizeMessageText(value) {
  if (typeof value !== "string") {
    throw new TypeError("message text must be a string");
  }
  const text = value.trim();
  if (text.length === 0) {
    throw new Error("message text must not be empty");
  }
  return text;
}

module.exports = {
  normalizeMessageText,
};
