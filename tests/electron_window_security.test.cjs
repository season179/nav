const assert = require("node:assert/strict");
const { test } = require("node:test");
const fs = require("node:fs");
const path = require("node:path");

const {
  createWindowOptions,
} = require("../desktop/electron/window-options.cjs");

test("Electron window keeps the renderer isolated from Node and Electron internals", () => {
  const options = createWindowOptions({ preloadPath: "/tmp/nav-preload.cjs" });

  assert.equal(options.titleBarStyle, "hidden");
  assert.deepEqual(options.trafficLightPosition, { x: 16, y: 18 });
  assert.equal(options.webPreferences.preload, "/tmp/nav-preload.cjs");
  assert.equal(options.webPreferences.contextIsolation, true);
  assert.equal(options.webPreferences.nodeIntegration, false);
  assert.equal(options.webPreferences.sandbox, true);
});

// Regression guard: the window above is sandboxed, and sandboxed preloads can
// only `require` `electron` plus a few builtins — never relative project files.
// A relative require throws at load, which silently drops `contextBridge`
// exposure so `window.nav` never appears and the chat composer stays disabled.
// (See the inlined validation in preload.cjs.)
test("sandboxed preload has no relative require() that would break window.nav", () => {
  const preloadPath = path.join(
    __dirname,
    "..",
    "desktop",
    "electron",
    "preload.cjs",
  );
  const source = fs.readFileSync(preloadPath, "utf8");
  // Strip line comments so the explanatory note in preload.cjs isn't matched.
  const code = source.replace(/\/\/.*$/gm, "");
  const relativeRequire = /require\(\s*['"]\.\.?\//.test(code);

  assert.equal(
    relativeRequire,
    false,
    "preload.cjs must be self-contained (no relative require) because the window is sandboxed",
  );
});
