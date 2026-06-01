const assert = require("node:assert/strict");
const { test } = require("node:test");

const {
  normalizeModelId,
  normalizeMessageText,
  normalizeModelProvider,
  normalizeOptionalSessionMode,
  normalizeOptionalWorkspaceRoot,
  normalizeSessionId,
} = require("../desktop/electron/request-validation.cjs");

test("preload message validation trims valid text", () => {
  assert.equal(normalizeMessageText("  hello  "), "hello");
});

test("preload message validation rejects non-strings", () => {
  assert.throws(() => normalizeMessageText(42), TypeError);
  assert.throws(() => normalizeMessageText({ text: "hi" }), TypeError);
  assert.throws(() => normalizeMessageText(undefined), TypeError);
});

test("preload message validation rejects blank text", () => {
  assert.throws(() => normalizeMessageText("   "));
  assert.throws(() => normalizeMessageText(""));
});

test("preload session id validation trims valid ids", () => {
  assert.equal(normalizeSessionId("  abc-123  "), "abc-123");
});

test("preload session id validation rejects non-strings and blanks", () => {
  assert.throws(() => normalizeSessionId(42), TypeError);
  assert.throws(() => normalizeSessionId(undefined), TypeError);
  assert.throws(() => normalizeSessionId("   "));
});

test("preload model switch validation trims provider and model ids", () => {
  assert.equal(normalizeModelProvider("  openai  "), "openai");
  assert.equal(normalizeModelId("  gpt-5.1  "), "gpt-5.1");
});

test("preload model switch validation rejects non-strings and blanks", () => {
  assert.throws(() => normalizeModelProvider(42), TypeError);
  assert.throws(() => normalizeModelProvider("   "));
  assert.throws(() => normalizeModelId(undefined), TypeError);
  assert.throws(() => normalizeModelId(""));
});

test("preload workspace root validation trims valid paths", () => {
  assert.equal(
    normalizeOptionalWorkspaceRoot("  /Users/season/Personal/nav  "),
    "/Users/season/Personal/nav",
  );
});

test("preload workspace root validation allows omitted values", () => {
  assert.equal(normalizeOptionalWorkspaceRoot(undefined), null);
  assert.equal(normalizeOptionalWorkspaceRoot(null), null);
});

test("preload workspace root validation rejects non-strings and blanks", () => {
  assert.throws(() => normalizeOptionalWorkspaceRoot(42), TypeError);
  assert.throws(() => normalizeOptionalWorkspaceRoot("   "));
});

test("preload session mode validation accepts local and worktree", () => {
  assert.equal(normalizeOptionalSessionMode("local"), "local");
  assert.equal(normalizeOptionalSessionMode("worktree"), "worktree");
  assert.equal(normalizeOptionalSessionMode(undefined), null);
  assert.equal(normalizeOptionalSessionMode(null), null);
});

test("preload session mode validation rejects unknown modes", () => {
  assert.throws(() => normalizeOptionalSessionMode("remote"));
  assert.throws(() => normalizeOptionalSessionMode(42));
});
