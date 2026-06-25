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
  const toolPath = path.join(
    REPO_ROOT,
    "desktop",
    "electron",
    "renderer",
    "src",
    "components",
    "ai-elements",
    "tool.tsx",
  );
  const codeBlockPath = path.join(
    REPO_ROOT,
    "desktop",
    "electron",
    "renderer",
    "src",
    "components",
    "ai-elements",
    "code-block.tsx",
  );
  const promptInputPath = path.join(
    REPO_ROOT,
    "desktop",
    "electron",
    "renderer",
    "src",
    "components",
    "ai-elements",
    "prompt-input.tsx",
  );
  const modelSelectorPath = path.join(
    REPO_ROOT,
    "desktop",
    "electron",
    "renderer",
    "src",
    "components",
    "ai-elements",
    "model-selector.tsx",
  );
  const contextPath = path.join(
    REPO_ROOT,
    "desktop",
    "electron",
    "renderer",
    "src",
    "components",
    "ai-elements",
    "context.tsx",
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
  const rootToolPath = path.join(
    REPO_ROOT,
    "components",
    "ai-elements",
    "tool.tsx",
  );
  const rootCodeBlockPath = path.join(
    REPO_ROOT,
    "components",
    "ai-elements",
    "code-block.tsx",
  );
  const rootPromptInputPath = path.join(
    REPO_ROOT,
    "components",
    "ai-elements",
    "prompt-input.tsx",
  );
  const rootModelSelectorPath = path.join(
    REPO_ROOT,
    "components",
    "ai-elements",
    "model-selector.tsx",
  );
  const rootContextPath = path.join(
    REPO_ROOT,
    "components",
    "ai-elements",
    "context.tsx",
  );

  const message = fs.readFileSync(messagePath, "utf8");
  const conversation = fs.readFileSync(conversationPath, "utf8");
  const tool = fs.readFileSync(toolPath, "utf8");
  const codeBlock = fs.readFileSync(codeBlockPath, "utf8");
  const promptInput = fs.readFileSync(promptInputPath, "utf8");
  const modelSelector = fs.readFileSync(modelSelectorPath, "utf8");
  const context = fs.readFileSync(contextPath, "utf8");
  assert.match(message, /export const MessageResponse/);
  assert.match(conversation, /export const Conversation/);
  assert.match(tool, /export const Tool/);
  assert.match(codeBlock, /export const CodeBlock/);
  assert.match(promptInput, /export const PromptInput/);
  assert.match(modelSelector, /export const ModelSelector/);
  assert.match(context, /export const Context/);
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
  assert.equal(
    fs.existsSync(rootToolPath),
    false,
    "AI Elements registry files should be relocated into renderer src",
  );
  assert.equal(
    fs.existsSync(rootCodeBlockPath),
    false,
    "AI Elements registry files should be relocated into renderer src",
  );
  assert.equal(
    fs.existsSync(rootPromptInputPath),
    false,
    "AI Elements registry files should be relocated into renderer src",
  );
  assert.equal(
    fs.existsSync(rootModelSelectorPath),
    false,
    "AI Elements registry files should be relocated into renderer src",
  );
  assert.equal(
    fs.existsSync(rootContextPath),
    false,
    "AI Elements registry files should be relocated into renderer src",
  );
});

test("Transcript renders assistant markdown through AI Elements MessageResponse", () => {
  const transcript = fs.readFileSync(
    path.join(
      REPO_ROOT,
      "desktop",
      "electron",
      "renderer",
      "src",
      "components",
      "Transcript.tsx",
    ),
    "utf8",
  );

  assert.match(transcript, /MessageResponse/);
  assert.doesNotMatch(transcript, /renderMarkdown|MarkdownText/);
});

test("Composer input is rebuilt on AI Elements PromptInput", () => {
  const composer = fs.readFileSync(
    path.join(
      REPO_ROOT,
      "desktop",
      "electron",
      "renderer",
      "src",
      "components",
      "Composer.tsx",
    ),
    "utf8",
  );

  assert.match(composer, /PromptInput/);
  assert.match(composer, /PromptInputTextarea/);
  assert.match(composer, /PromptInputSubmit/);
  assert.doesNotMatch(composer, /<textarea|composer-row/);
});

test("Composer metadata uses AI Elements model selector and context", () => {
  const composer = fs.readFileSync(
    path.join(
      REPO_ROOT,
      "desktop",
      "electron",
      "renderer",
      "src",
      "components",
      "Composer.tsx",
    ),
    "utf8",
  );

  assert.match(composer, /ModelSelector/);
  assert.match(composer, /ModelSelectorInput/);
  assert.match(composer, /TokenContext/);
  assert.match(composer, /ContextContentHeader/);
  assert.doesNotMatch(composer, /model-search|filterModelOptions/);
});
