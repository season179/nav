const assert = require("node:assert/strict");
const path = require("node:path");
const { test } = require("node:test");
const { pathToFileURL } = require("node:url");

test("project groups keep their existing order when a nested session becomes newest", async () => {
  const { groupSessionsByProject } = await loadProjectList();
  const sessions = [
    { sessionId: "b-old", workspaceRoot: "/work/project-b", updatedAt: 100 },
    { sessionId: "a-old", workspaceRoot: "/work/project-a", updatedAt: 200 },
    { sessionId: "b-new", workspaceRoot: "/work/project-b", updatedAt: 300 },
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
    "sessions inside a project sort by latest activity",
  );
});

test("sessions with invalid activity sort as zero while preserving tie order", async () => {
  const { groupSessionsByProject } = await loadProjectList();
  const sessions = [
    { sessionId: "a", workspaceRoot: "/work/project-a", updatedAt: undefined },
    { sessionId: "b", workspaceRoot: "/work/project-a", updatedAt: "invalid" },
    { sessionId: "c", workspaceRoot: "/work/project-a", updatedAt: 100 },
    { sessionId: "d", workspaceRoot: "/work/project-b", updatedAt: null },
    { sessionId: "e", workspaceRoot: "/work/project-b", updatedAt: 1 },
  ];

  const projects = groupSessionsByProject(sessions);

  assert.deepEqual(
    projects[0].sessions.map((session) => session.sessionId),
    ["c", "a", "b"],
  );
  assert.deepEqual(
    projects[1].sessions.map((session) => session.sessionId),
    ["e", "d"],
  );
});

test("new project groups append after already-known projects", async () => {
  const { groupSessionsByProject } = await loadProjectList();
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

test("project paths normalize into stable keys", async () => {
  const { normalizeProjectPath, projectKey } = await loadProjectList();
  assert.equal(
    normalizeProjectPath("  C:\\Users\\season\\nav\\\\  "),
    "C:/Users/season/nav",
  );
  assert.equal(projectKey(normalizeProjectPath("   ")), "__no_project__");
});

function loadProjectList() {
  return import(
    pathToFileURL(
      path.join(
        __dirname,
        "../desktop/electron/renderer/src/lib/project-list.mjs",
      ),
    ).href
  );
}
