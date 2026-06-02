const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const {
  DEFAULT_SESSION_MODE,
  coerceSessionMode,
  readSessionMode,
  writeSessionMode,
} = require("../desktop/electron/session-mode-store.cjs");

function tempFile() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "nav-session-mode-"));
  return path.join(dir, "nav-ui.json");
}

test("session mode coerces to worktree only for the exact value", () => {
  assert.equal(coerceSessionMode("worktree"), "worktree");
  assert.equal(coerceSessionMode("local"), "local");
  assert.equal(coerceSessionMode("WORKTREE"), DEFAULT_SESSION_MODE);
  assert.equal(coerceSessionMode(undefined), DEFAULT_SESSION_MODE);
  assert.equal(coerceSessionMode(42), DEFAULT_SESSION_MODE);
});

test("reading a missing preference file defaults to local", () => {
  const filePath = tempFile();
  assert.equal(readSessionMode(filePath), "local");
});

test("writing then reading the preference round-trips the mode", () => {
  const filePath = tempFile();
  assert.equal(writeSessionMode(filePath, "worktree"), "worktree");
  assert.equal(readSessionMode(filePath), "worktree");
  assert.equal(writeSessionMode(filePath, "local"), "local");
  assert.equal(readSessionMode(filePath), "local");
});

test("writing an invalid mode persists the safe local default", () => {
  const filePath = tempFile();
  assert.equal(writeSessionMode(filePath, "remote"), "local");
  assert.equal(readSessionMode(filePath), "local");
});

test("a corrupt preference file falls back to local rather than throwing", () => {
  const filePath = tempFile();
  fs.writeFileSync(filePath, "{ this is not json");
  assert.equal(readSessionMode(filePath), "local");
});

test("a preference file with an unexpected shape falls back to local", () => {
  const filePath = tempFile();
  fs.writeFileSync(filePath, JSON.stringify({ newSessionMode: "remote" }));
  assert.equal(readSessionMode(filePath), "local");
  fs.writeFileSync(filePath, JSON.stringify({ other: "worktree" }));
  assert.equal(readSessionMode(filePath), "local");
});
