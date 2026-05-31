const assert = require("node:assert/strict");
const { test } = require("node:test");

const { normalizeMessageText } = require("../desktop/electron/request-validation.cjs");

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
