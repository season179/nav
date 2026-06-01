const assert = require("node:assert/strict");
const path = require("node:path");
const { test } = require("node:test");
const { pathToFileURL } = require("node:url");

test("project sidebar shows five sessions before revealing older sessions", async () => {
  const { projectSessionView } = await loadSidebarModel();
  const project = projectWithSessions(7);

  const collapsed = projectSessionView(project, false);
  assert.deepEqual(
    collapsed.visibleSessions.map((session) => session.title),
    ["Session 1", "Session 2", "Session 3", "Session 4", "Session 5"],
  );
  assert.equal(collapsed.toggle.label, "Show more");
  assert.equal(collapsed.toggle.ariaLabel, "Show 2 more sessions in nav");

  const expanded = projectSessionView(project, true);
  assert.deepEqual(
    expanded.visibleSessions.map((session) => session.title),
    project.sessions.map((session) => session.title),
  );
  assert.equal(expanded.toggle.label, "Show less");
  assert.equal(expanded.toggle.ariaLabel, "Show fewer sessions in nav");
});

test("project sidebar omits session toggle for five sessions", async () => {
  const { projectSessionView } = await loadSidebarModel();
  const project = projectWithSessions(5);

  const view = projectSessionView(project, false);
  assert.equal(view.visibleSessions.length, 5);
  assert.equal(view.toggle, null);
});

test("expanded project sidebar renders at most twenty sessions", async () => {
  const { projectSessionView } = await loadSidebarModel();
  const project = projectWithSessions(25);

  const collapsed = projectSessionView(project, false);
  assert.equal(collapsed.toggle.ariaLabel, "Show 15 more sessions in nav");

  const expanded = projectSessionView(project, true);
  assert.deepEqual(
    expanded.visibleSessions.map((session) => session.title),
    sessionRange(20).map((session) => session.title),
  );
  assert.equal(project.sessions.length, 25, "older sessions remain available");
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
    projectSessionToggle(project, false).ariaLabel,
    "Show 2 more sessions in nav (Personal/nav)",
  );
});

function loadSidebarModel() {
  return import(
    pathToFileURL(
      path.join(
        __dirname,
        "../desktop/electron/renderer/src/components/sidebar-model.mjs",
      ),
    ).href
  );
}

function sessionRange(count) {
  return Array.from({ length: count }, (_, index) => ({
    sessionId: `session-${index + 1}`,
    title: `Session ${index + 1}`,
  }));
}

function projectWithSessions(count) {
  return {
    key: "/projects/nav",
    path: "/projects/nav",
    name: "nav",
    sessions: sessionRange(count),
  };
}
