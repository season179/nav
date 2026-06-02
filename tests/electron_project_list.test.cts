const assert = require("node:assert/strict");
const { test } = require("node:test");

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

test("worktree sessions group under their project root", async () => {
  const { groupSessionsByProject } = await loadProjectList();
  const sessions = [
    {
      sessionId: "main",
      workspaceRoot: "/Users/season/Personal/nav",
      projectRoot: "/Users/season/Personal/nav",
      updatedAt: 100,
    },
    {
      sessionId: "worktree",
      workspaceRoot: "/Users/season/.codex/worktrees/8f49/nav",
      projectRoot: "/Users/season/Personal/nav",
      updatedAt: 200,
    },
  ];

  const projects = groupSessionsByProject(sessions);

  assert.equal(projects.length, 1);
  assert.equal(projects[0].path, "/Users/season/Personal/nav");
  assert.deepEqual(
    projects[0].sessions.map((session) => session.sessionId),
    ["worktree", "main"],
  );
});

test("same-named non-worktree project groups expose distinguishing path hints", async () => {
  const { groupSessionsByProject } = await loadProjectList();
  const sessions = [
    {
      sessionId: "personal",
      workspaceRoot: "/Users/season/Personal/nav",
      updatedAt: 200,
    },
    {
      sessionId: "archive",
      workspaceRoot: "/Users/season/Archive/nav",
      updatedAt: 100,
    },
    {
      sessionId: "experiments",
      workspaceRoot: "/Users/season/Experiments",
      updatedAt: 50,
    },
  ];

  const projects = groupSessionsByProject(sessions);

  assert.deepEqual(
    projects.map((project) => ({
      displayName: project.displayName,
      pathHint: project.pathHint,
    })),
    [
      { displayName: "nav (Personal/nav)", pathHint: "Personal/nav" },
      { displayName: "nav (Archive/nav)", pathHint: "Archive/nav" },
      { displayName: "Experiments", pathHint: null },
    ],
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
  return import("../desktop/electron/renderer/src/lib/project-list.ts");
}
