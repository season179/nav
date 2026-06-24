const assert = require("node:assert/strict");
const { test } = require("node:test");

test("settings model helpers derive stable model keys and searchable text", async () => {
  const {
    modelInfoKey,
    modelOptionKey,
    modelOptionMatchesQuery,
    settingsFormDefaults,
  } = await loadSettingsModel();
  const option = {
    provider: "openai",
    model: "gpt-5.1",
    label: "GPT-5.1",
    thinkingLevels: ["off", "low"],
  };

  assert.equal(modelOptionKey(option), "openai:gpt-5.1");
  assert.equal(
    modelInfoKey({ label: "GPT-5.1", provider: "openai", model: "gpt-5.1" }),
    "openai:gpt-5.1",
  );
  assert.equal(modelOptionMatchesQuery(option, "openai"), true);
  assert.equal(modelOptionMatchesQuery(option, "claude"), false);
  assert.deepEqual(
    settingsFormDefaults("worktree", {
      label: "GPT-5.1",
      provider: "openai",
      model: "gpt-5.1",
      thinking: "low",
      thinkingLevels: ["off", "low"],
    }),
    {
      mode: "worktree",
      modelKey: "openai:gpt-5.1",
      thinking: "low",
    },
  );
});

test("settings helpers tolerate incomplete model info", async () => {
  const { modelInfoKey, settingsFormDefaults, thinkingLevelsFor } =
    await loadSettingsModel();

  assert.equal(modelInfoKey({ label: "Unknown" }), "");
  assert.deepEqual(settingsFormDefaults("local", null), {
    mode: "local",
    modelKey: "",
    thinking: "",
  });
  assert.deepEqual(
    settingsFormDefaults("local", {
      label: "Unknown",
      thinkingLevels: ["off", "low"],
    }),
    {
      mode: "local",
      modelKey: "",
      thinking: "off",
    },
  );
  assert.deepEqual(thinkingLevelsFor({ label: "Unknown" }), []);
});

function loadSettingsModel() {
  return import("../desktop/electron/renderer/src/lib/settings-model.ts");
}
