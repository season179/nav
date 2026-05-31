const assert = require("node:assert/strict");
const { test } = require("node:test");

const {
  normalizeMessageText,
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
