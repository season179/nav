const assert = require("node:assert/strict");
const { test } = require("node:test");

test("composer draft storage uses one key per session", async () => {
  const { composerDraftStorageKey, readComposerDraft, writeComposerDraft } =
    await loadComposerDraft();
  const storage = memoryStorage();

  writeComposerDraft(storage, "session 1", "first draft");
  writeComposerDraft(storage, "session/2", "second draft");

  assert.equal(
    composerDraftStorageKey("session/2"),
    "nav.composerDraft.v1:session%2F2",
  );
  assert.equal(readComposerDraft(storage, "session 1"), "first draft");
  assert.equal(readComposerDraft(storage, "session/2"), "second draft");
});

test("empty composer drafts are removed instead of persisted", async () => {
  const { readComposerDraft, writeComposerDraft } = await loadComposerDraft();
  const storage = memoryStorage();

  writeComposerDraft(storage, "session 1", "draft");
  writeComposerDraft(storage, "session 1", "");

  assert.equal(readComposerDraft(storage, "session 1"), "");
  assert.equal(storage.size, 0);
});

test("composer draft storage failures are ignored", async () => {
  const { readComposerDraft, writeComposerDraft } = await loadComposerDraft();
  const storage = failingStorage();

  assert.doesNotThrow(() => writeComposerDraft(storage, "session 1", "draft"));
  assert.equal(readComposerDraft(storage, "session 1"), "");
});

test("composer message validation rejects blank and disconnected submits", async () => {
  const { normalizeComposerMessage, validateComposerMessage } =
    await loadComposerValidation();

  assert.equal(normalizeComposerMessage("  ship it  "), "ship it");
  assert.equal(validateComposerMessage("  ", true), "Message is required");
  assert.equal(
    validateComposerMessage("ship it", false),
    "Backend is not connected",
  );
  assert.equal(validateComposerMessage("ship it", true), undefined);
});

function loadComposerDraft() {
  return import("../desktop/electron/renderer/src/lib/composer-draft.ts");
}

function loadComposerValidation() {
  return import("../desktop/electron/renderer/src/lib/composer-validation.ts");
}

function memoryStorage(): TestStorage {
  const values = new Map<string, string>();
  return {
    get size() {
      return values.size;
    },
    getItem(key: string) {
      return values.get(key) ?? null;
    },
    removeItem(key: string) {
      values.delete(key);
    },
    setItem(key: string, value: string) {
      values.set(key, value);
    },
  };
}

function failingStorage(): TestStorage {
  return {
    getItem() {
      throw new Error("get failed");
    },
    removeItem() {
      throw new Error("remove failed");
    },
    setItem() {
      throw new Error("set failed");
    },
  };
}

type TestStorage = {
  readonly size?: number;
  getItem: (key: string) => string | null;
  removeItem: (key: string) => void;
  setItem: (key: string, value: string) => void;
};
