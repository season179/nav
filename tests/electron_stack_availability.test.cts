const assert = require("node:assert/strict");
const { test } = require("node:test");

test("terminal run events refresh stack availability", async () => {
  const {
    STACK_AVAILABILITY_RECHECK_DELAY_MS,
    shouldRefreshStackAvailabilityForEvent,
  } = await loadStackAvailability();

  assert.equal(STACK_AVAILABILITY_RECHECK_DELAY_MS, 120);
  assert.equal(shouldRefreshStackAvailabilityForEvent("run.completed"), true);
  assert.equal(shouldRefreshStackAvailabilityForEvent("run.failed"), true);
  assert.equal(shouldRefreshStackAvailabilityForEvent("run.cancelled"), true);
});

test("non-terminal run events do not schedule stack availability checks", async () => {
  const { shouldRefreshStackAvailabilityForEvent } =
    await loadStackAvailability();

  assert.equal(
    shouldRefreshStackAvailabilityForEvent("message.completed"),
    false,
  );
  assert.equal(shouldRefreshStackAvailabilityForEvent("tool.completed"), false);
  assert.equal(
    shouldRefreshStackAvailabilityForEvent("assistant.tool_calls"),
    false,
  );
});

function loadStackAvailability() {
  return import("../desktop/electron/renderer/src/lib/stack-availability.ts");
}
