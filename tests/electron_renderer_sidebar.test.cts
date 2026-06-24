const assert = require("node:assert/strict");
const { test } = require("node:test");

test("project sidebar shows five sessions before revealing older sessions", async () => {
  const { projectSessionView } = await loadSidebarModel();
  const project = projectWithSessions(7);

  const collapsed = projectSessionView(project, false);
  assert.deepEqual(
    collapsed.visibleSessions.map((session) => session.title),
    ["Session 1", "Session 2", "Session 3", "Session 4", "Session 5"],
  );
  assert.equal(collapsed.toggle?.label, "Show more");
  assert.equal(collapsed.toggle?.ariaLabel, "Show 2 more sessions in nav");

  const expanded = projectSessionView(project, true);
  assert.deepEqual(
    expanded.visibleSessions.map((session) => session.title),
    project.sessions.map((session) => session.title),
  );
  assert.equal(expanded.toggle?.label, "Show less");
  assert.equal(expanded.toggle?.ariaLabel, "Show fewer sessions in nav");
});

test("project sidebar omits session toggle for five sessions", async () => {
  const { projectSessionView } = await loadSidebarModel();
  const project = projectWithSessions(5);

  const view = projectSessionView(project, false);
  assert.equal(view.visibleSessions.length, 5);
  assert.equal(view.toggle, null);
});

test("expanded project sidebar exposes every session for virtualization", async () => {
  const { projectSessionView, shouldVirtualizeProjectSessions } =
    await loadSidebarModel();
  const project = projectWithSessions(25);

  const collapsed = projectSessionView(project, false);
  assert.equal(collapsed.toggle?.ariaLabel, "Show 20 more sessions in nav");

  const expanded = projectSessionView(project, true);
  assert.deepEqual(
    expanded.visibleSessions.map((session) => session.title),
    project.sessions.map((session) => session.title),
  );
  assert.equal(
    shouldVirtualizeProjectSessions(expanded.visibleSessions.length),
    true,
  );
});

test("short project sidebar does not use the virtualized path", async () => {
  const { shouldVirtualizeProjectSessions } = await loadSidebarModel();

  assert.equal(shouldVirtualizeProjectSessions(12), false);
  assert.equal(shouldVirtualizeProjectSessions(13), true);
});

test("project heading exposes collapse and expand state", async () => {
  const { projectToggleView } = await loadSidebarModel();
  const project = projectWithSessions(3);

  assert.deepEqual(projectToggleView(project, false), {
    ariaExpanded: "true",
    ariaLabel: "Collapse project nav",
    disclosure: "v",
  });
  assert.deepEqual(projectToggleView(project, true), {
    ariaExpanded: "false",
    ariaLabel: "Expand project nav",
    disclosure: ">",
  });
});

test("project aria labels use a disambiguated display name", async () => {
  const { projectSessionToggle, projectToggleView } = await loadSidebarModel();
  const project = {
    ...projectWithSessions(7),
    displayName: "nav (Personal/nav)",
    pathHint: "Personal/nav",
  };

  assert.equal(
    projectToggleView(project, false).ariaLabel,
    "Collapse project nav (Personal/nav)",
  );
  assert.equal(
    projectSessionToggle(project, false)?.ariaLabel,
    "Show 2 more sessions in nav (Personal/nav)",
  );
});

function loadSidebarModel() {
  return import("../desktop/electron/renderer/src/components/sidebar-model.ts");
}

function sessionRange(count: number) {
  return Array.from({ length: count }, (_, index) => ({
    sessionId: `session-${index + 1}`,
    title: `Session ${index + 1}`,
  }));
}

function projectWithSessions(count: number) {
  return {
    key: "/projects/nav",
    path: "/projects/nav",
    name: "nav",
    sessions: sessionRange(count),
  };
}
