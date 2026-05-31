const assert = require("node:assert/strict");
const { test } = require("node:test");

const { createWindowOptions } = require("../desktop/electron/window-options.cjs");

test("Electron window keeps the renderer isolated from Node and Electron internals", () => {
  const options = createWindowOptions({ preloadPath: "/tmp/nav-preload.cjs" });

  assert.equal(options.webPreferences.preload, "/tmp/nav-preload.cjs");
  assert.equal(options.webPreferences.contextIsolation, true);
  assert.equal(options.webPreferences.nodeIntegration, false);
  assert.equal(options.webPreferences.sandbox, true);
});
