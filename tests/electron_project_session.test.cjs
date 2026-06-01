const assert = require("node:assert/strict");
const { test } = require("node:test");

const {
  existingProjectSessionId,
} = require("../desktop/electron/project-session.cjs");

test("project creation reuses the newest session for an existing workspace", () => {
  const sessions = [
    {
      sessionId: "older",
      workspaceRoot: "/Users/season/Personal/nav/",
      updatedAt: 100,
    },
    {
      sessionId: "newest",
      workspaceRoot: "/Users/season/Personal/nav",
      updatedAt: 300,
    },
    {
      sessionId: "other",
      workspaceRoot: "/Users/season/Personal/other",
      updatedAt: 500,
    },
  ];

  assert.equal(
    existingProjectSessionId(sessions, "/Users/season/Personal/nav"),
    "newest",
  );
});

test("project creation treats slash variants as the same workspace", () => {
  const sessions = [
    {
      sessionId: "existing",
      workspaceRoot: "C:\\Users\\season\\nav\\",
      updatedAt: 100,
    },
  ];

  assert.equal(
    existingProjectSessionId(sessions, "C:/Users/season/nav"),
    "existing",
  );
});

test("project creation allows a workspace that has not been added yet", () => {
  assert.equal(
    existingProjectSessionId(
      [{ sessionId: "existing", workspaceRoot: "/projects/nav" }],
      "/projects/pi",
    ),
    null,
  );
});
