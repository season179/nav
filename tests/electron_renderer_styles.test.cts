const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

test("session mode trigger keeps a stable slot for local and worktree labels", () => {
  const styles = readRendererStyles();

  const triggerRule = cssRule(styles, ".session-mode-trigger");
  assert.match(triggerRule, /justify-content:\s*space-between;/);
  assert.match(triggerRule, /width:\s*96px;/);
});

test("sidebar uses a flat opaque base color", () => {
  const styles = readRendererStyles();

  const rootRule = cssRule(styles, ":root");
  const sidebarRule = cssRule(styles, ".sidebar");
  const newChatRule = cssRule(styles, ".new-chat");

  assert.match(rootRule, /--sidebar-bg:\s*oklch\(0\.35 0\.024 266\);/);
  assert.match(rootRule, /--sidebar-glass:/);
  assert.doesNotMatch(rootRule, /--sidebar-glass-highlight-start:/);
  assert.doesNotMatch(rootRule, /--sidebar-glass-highlight-mid:/);
  assert.doesNotMatch(rootRule, /--sidebar-glass-shadow:/);
  assert.match(sidebarRule, /background:\s*var\(--sidebar-bg\);/);
  assert.doesNotMatch(sidebarRule, /linear-gradient\(/);
  assert.match(newChatRule, /background:\s*var\(--sidebar-glass-strong\);/);
});

function readRendererStyles() {
  const entry = path.join(
    __dirname,
    "..",
    "desktop",
    "electron",
    "renderer",
    "styles.css",
  );
  return resolveCssImports(entry);
}

// styles.css is an @import manifest; the rules live in feature partials. Inline
// the imports in order (mirroring how Vite bundles them) so assertions can match
// a rule wherever it lives.
function resolveCssImports(filePath: string, seen: Set<string> = new Set()) {
  if (seen.has(filePath)) {
    return "";
  }
  seen.add(filePath);
  const source = fs.readFileSync(filePath, "utf8");
  const dir = path.dirname(filePath);
  return source.replace(
    /@import\s+["']([^"']+)["'];?/g,
    (_match: string, ref: string) =>
      resolveCssImports(path.resolve(dir, ref), seen),
  );
}

function cssRule(styles: string, selector: string) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = styles.match(
    new RegExp(`${escapedSelector}\\s*\\{([\\s\\S]*?)\\}`),
  );
  if (!match) {
    throw new Error(`${selector} rule should exist`);
  }
  return match[1];
}
