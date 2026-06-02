const assert = require("node:assert/strict");
const { test } = require("node:test");

const {
  isExternalBrowserUrl,
} = require("../desktop/electron/out/external-links.cjs");

test("Electron opens ordinary web links in the external browser", () => {
  assert.equal(isExternalBrowserUrl("https://example.com/path?q=1"), true);
  assert.equal(isExternalBrowserUrl("http://example.com"), true);
});

test("Electron does not externalize non-web or invalid links", () => {
  assert.equal(isExternalBrowserUrl("file:///tmp/index.html"), false);
  assert.equal(isExternalBrowserUrl("javascript:alert(1)"), false);
  assert.equal(isExternalBrowserUrl("mailto:hello@example.com"), false);
  assert.equal(isExternalBrowserUrl("not a url"), false);
});
