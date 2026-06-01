const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

test("session mode trigger keeps a stable slot for local and worktree labels", () => {
  const styles = fs.readFileSync(
    path.join(__dirname, "..", "desktop", "electron", "renderer", "styles.css"),
    "utf8",
  );

  const triggerRule = cssRule(styles, ".session-mode-trigger");
  assert.match(triggerRule, /justify-content:\s*space-between;/);
  assert.match(triggerRule, /width:\s*96px;/);
});

function cssRule(styles, selector) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = styles.match(
    new RegExp(`${escapedSelector}\\s*\\{([\\s\\S]*?)\\}`),
  );
  assert.ok(match, `${selector} rule should exist`);
  return match[1];
}
