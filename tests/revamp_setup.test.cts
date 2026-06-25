const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const REPO_ROOT = path.resolve(__dirname, "..");

test("styles.css first @import is ./styles/theme.css", () => {
  const stylesCss = fs.readFileSync(
    path.join(REPO_ROOT, "desktop", "electron", "renderer", "styles.css"),
    "utf8",
  );
  const firstImport = stylesCss.match(/@import\s+["'][^"']+["']/);
  assert.ok(firstImport, "expected at least one @import in styles.css");
  assert.match(firstImport[0], /@import\s+["'].\/styles\/theme.css["']/);
});

test("theme.css contains @import tailwindcss, --primary, and streamdown", () => {
  const themeCss = fs.readFileSync(
    path.join(
      REPO_ROOT,
      "desktop",
      "electron",
      "renderer",
      "styles",
      "theme.css",
    ),
    "utf8",
  );
  assert.match(themeCss, /@import\s+["']tailwindcss["']/);
  assert.match(themeCss, /--primary:/);
  assert.match(themeCss, /streamdown/);
});

test("root components.json exists", () => {
  const componentsJson = fs.readFileSync(
    path.join(REPO_ROOT, "components.json"),
    "utf8",
  );
  const config = JSON.parse(componentsJson);
  assert.equal(config.style, "new-york");
  assert.equal(config.rsc, false);
  assert.equal(config.tailwind.cssVariables, true);
});

test("AI Elements generated components live under renderer src", () => {
  const messagePath = path.join(
    REPO_ROOT,
    "desktop",
    "electron",
    "renderer",
    "src",
    "components",
    "ai-elements",
    "message.tsx",
  );
  const conversationPath = path.join(
    REPO_ROOT,
    "desktop",
    "electron",
    "renderer",
    "src",
    "components",
    "ai-elements",
    "conversation.tsx",
  );
  const rootMessagePath = path.join(
    REPO_ROOT,
    "components",
    "ai-elements",
    "message.tsx",
  );
  const rootConversationPath = path.join(
    REPO_ROOT,
    "components",
    "ai-elements",
    "conversation.tsx",
  );

  const message = fs.readFileSync(messagePath, "utf8");
  const conversation = fs.readFileSync(conversationPath, "utf8");
  assert.match(message, /export const MessageResponse/);
  assert.match(conversation, /export const Conversation/);
  assert.equal(
    fs.existsSync(rootMessagePath),
    false,
    "AI Elements registry files should be relocated into renderer src",
  );
  assert.equal(
    fs.existsSync(rootConversationPath),
    false,
    "AI Elements registry files should be relocated into renderer src",
  );
});
