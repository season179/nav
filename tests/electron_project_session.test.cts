const assert = require("node:assert/strict");
const { test } = require("node:test");

const {
  existingProjectSessionId,
} = require("../desktop/electron/out/project-session.cjs");

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

test("worktree project creation does not reuse a local session for the same checkout", () => {
  const sessions = [
    {
      sessionId: "local",
      workspaceRoot: "/Users/season/Personal/nav",
      projectRoot: "/Users/season/Personal/nav",
      updatedAt: 500,
    },
  ];

  assert.equal(
    existingProjectSessionId(
      sessions,
      "/Users/season/Personal/nav",
      "worktree",
    ),
    null,
  );
});

test("worktree project creation reuses the newest worktree for the checkout", () => {
  const sessions = [
    {
      sessionId: "local",
      workspaceRoot: "/Users/season/Personal/nav",
      projectRoot: "/Users/season/Personal/nav",
      updatedAt: 500,
    },
    {
      sessionId: "older-worktree",
      workspaceRoot: "/Users/season/Personal/nav/.nav/worktrees/nav-wt-old",
      projectRoot: "/Users/season/Personal/nav",
      updatedAt: 300,
    },
    {
      sessionId: "newer-worktree",
      workspaceRoot: "/Users/season/Personal/nav/.nav/worktrees/nav-wt-new",
      projectRoot: "/Users/season/Personal/nav",
      updatedAt: 700,
    },
  ];

  assert.equal(
    existingProjectSessionId(
      sessions,
      "/Users/season/Personal/nav",
      "worktree",
    ),
    "newer-worktree",
  );
});

test("worktree startup resumes a worktree session for the checkout, never the local one", () => {
  // Mirrors the launch-time selection in main.cjs `openSession`: when the
  // persisted mode is "worktree", startup must reopen a worktree session for
  // PROJECT_ROOT and must not fall back to the main-checkout (local) session.
  const projectRoot = "/Users/season/Personal/nav";
  const sessions = [
    {
      sessionId: "startup-local",
      workspaceRoot: projectRoot,
      projectRoot,
      updatedAt: 900,
    },
    {
      sessionId: "startup-worktree",
      workspaceRoot: `${projectRoot}/.nav/worktrees/nav-wt-019e8685`,
      projectRoot,
      updatedAt: 400,
    },
  ];

  assert.equal(
    existingProjectSessionId(sessions, projectRoot, "worktree"),
    "startup-worktree",
  );
});

test("worktree project creation ignores a session with a missing or empty workspaceRoot", () => {
  const sessions = [
    {
      sessionId: "no-workspace",
      projectRoot: "/Users/season/Personal/nav",
      updatedAt: 700,
    },
    {
      sessionId: "empty-workspace",
      workspaceRoot: "",
      projectRoot: "/Users/season/Personal/nav",
      updatedAt: 700,
    },
  ];

  assert.equal(
    existingProjectSessionId(
      sessions,
      "/Users/season/Personal/nav",
      "worktree",
    ),
    null,
  );
});
