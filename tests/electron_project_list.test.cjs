const assert = require("node:assert/strict");
const { test } = require("node:test");

const {
  groupSessionsByProject,
  normalizeProjectPath,
  projectKey,
} = require("../desktop/electron/renderer/project-list.js");

test("project groups keep their existing order when a nested session becomes newest", () => {
  const sessions = [
    { sessionId: "b-new", workspaceRoot: "/work/project-b", updatedAt: 300 },
    { sessionId: "a-old", workspaceRoot: "/work/project-a", updatedAt: 200 },
    { sessionId: "b-old", workspaceRoot: "/work/project-b", updatedAt: 100 },
  ];

  const projects = groupSessionsByProject(sessions, [
    "/work/project-a",
    "/work/project-b",
  ]);

  assert.deepEqual(
    projects.map((project) => project.path),
    ["/work/project-a", "/work/project-b"],
  );
  assert.deepEqual(
    projects[1].sessions.map((session) => session.sessionId),
    ["b-new", "b-old"],
    "sessions inside a project still keep backend recency order",
  );
});

test("new project groups append after already-known projects", () => {
  const sessions = [
    { sessionId: "c", workspaceRoot: "/work/project-c", updatedAt: 300 },
    { sessionId: "a", workspaceRoot: "/work/project-a", updatedAt: 200 },
    { sessionId: "b", workspaceRoot: "/work/project-b", updatedAt: 100 },
  ];

  const projects = groupSessionsByProject(sessions, [
    "/work/project-a",
    "/work/project-b",
  ]);

  assert.deepEqual(
    projects.map((project) => project.path),
    ["/work/project-a", "/work/project-b", "/work/project-c"],
  );
});

test("project paths normalize into stable keys", () => {
  assert.equal(
    normalizeProjectPath("  C:\\Users\\season\\nav\\\\  "),
    "C:/Users/season/nav",
  );
  assert.equal(projectKey(normalizeProjectPath("   ")), "__no_project__");
});
