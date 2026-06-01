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

test("sidebar keeps an opaque base with a glass treatment", () => {
  const styles = readRendererStyles();

  const rootRule = cssRule(styles, ":root");
  const sidebarRule = cssRule(styles, ".sidebar");
  const newChatRule = cssRule(styles, ".new-chat");

  assert.match(rootRule, /--sidebar-bg:\s*oklch\(0\.28 0 195\);/);
  assert.match(rootRule, /--sidebar-glass:/);
  assert.match(
    rootRule,
    /--sidebar-backdrop-filter:\s*blur\(18px\) saturate\(1\.08\);/,
  );
  assert.match(sidebarRule, /linear-gradient\(/);
  assert.match(sidebarRule, /var\(--sidebar-bg\);/);
  assert.match(
    sidebarRule,
    /backdrop-filter:\s*var\(--sidebar-backdrop-filter\);/,
  );
  assert.match(newChatRule, /background:\s*var\(--sidebar-glass-strong\);/);
});

function readRendererStyles() {
  return fs.readFileSync(
    path.join(__dirname, "..", "desktop", "electron", "renderer", "styles.css"),
    "utf8",
  );
}

function cssRule(styles, selector) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = styles.match(
    new RegExp(`${escapedSelector}\\s*\\{([\\s\\S]*?)\\}`),
  );
  assert.ok(match, `${selector} rule should exist`);
  return match[1];
}
