const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

const projectList = require("../desktop/electron/renderer/project-list.js");

test("project sidebar shows five sessions before revealing older sessions", () => {
  const renderer = loadRenderer();
  const project = projectWithSessions(7);

  const collapsed = renderer.renderProjectSessions(project);
  assert.deepEqual(sessionButtonLabels(collapsed), [
    "Session 1",
    "Session 2",
    "Session 3",
    "Session 4",
    "Session 5",
  ]);

  const showMore = findByClass(collapsed, "project-session-toggle");
  assert.ok(
    showMore,
    "projects with more than five sessions get a reveal button",
  );
  assert.equal(showMore.textContent, "Show more");
  assert.equal(
    showMore.getAttribute("aria-label"),
    "Show 2 more sessions in nav",
  );

  showMore.click();
  const expanded = renderer.renderProjectSessions(project);
  assert.deepEqual(
    sessionButtonLabels(expanded),
    project.sessions.map((session) => session.title),
  );
  const showLess = findByClass(expanded, "project-session-toggle");
  assert.ok(showLess, "expanded projects get a collapse button");
  assert.equal(showLess.textContent, "Show less");
  assert.equal(
    showLess.getAttribute("aria-label"),
    "Show fewer sessions in nav",
  );

  showLess.click();
  const collapsedAgain = renderer.renderProjectSessions(project);
  assert.deepEqual(sessionButtonLabels(collapsedAgain), [
    "Session 1",
    "Session 2",
    "Session 3",
    "Session 4",
    "Session 5",
  ]);
});

test("project sidebar omits session toggle for five sessions", () => {
  const renderer = loadRenderer();
  const project = projectWithSessions(5);

  const list = renderer.renderProjectSessions(project);
  assert.equal(sessionButtonLabels(list).length, 5);
  assert.equal(findByClass(list, "project-session-toggle"), null);
});

test("expanded project sidebar renders at most twenty sessions", () => {
  const renderer = loadRenderer();
  const project = projectWithSessions(25);

  const collapsed = renderer.renderProjectSessions(project);
  const showMore = findByClass(collapsed, "project-session-toggle");
  assert.equal(
    showMore.getAttribute("aria-label"),
    "Show 15 more sessions in nav",
  );

  showMore.click();
  const expanded = renderer.renderProjectSessions(project);
  assert.deepEqual(
    sessionButtonLabels(expanded),
    sessionRange(20).map((session) => session.title),
  );
  assert.equal(project.sessions.length, 25, "older sessions remain available");
});

test("clicking a project heading collapses and expands its sessions", async () => {
  const sessions = sessionRange(3).map((session) => ({
    ...session,
    workspaceRoot: "/projects/nav",
  }));
  const renderer = loadRenderer({ sessions });

  await renderer.refreshSessions();

  const sessionList = renderer.document.querySelector("#session-list");
  assert.deepEqual(sessionButtonLabels(sessionList), [
    "Session 1",
    "Session 2",
    "Session 3",
  ]);

  let projectToggle = findByClass(sessionList, "project-toggle");
  assert.ok(projectToggle, "project heading renders as a toggle");
  assert.equal(projectToggle.getAttribute("aria-expanded"), "true");
  assert.equal(
    projectToggle.getAttribute("aria-label"),
    "Collapse project nav",
  );

  const projectChatAdd = findByClass(sessionList, "project-chat-add");
  assert.ok(projectChatAdd, "project heading keeps a separate new-chat button");
  assert.notStrictEqual(projectChatAdd, projectToggle);
  assert.equal(projectChatAdd.type, "button");
  assert.equal(projectChatAdd.getAttribute("aria-label"), "New chat in nav");

  projectToggle.click();

  assert.deepEqual(sessionButtonLabels(sessionList), []);
  projectToggle = findByClass(sessionList, "project-toggle");
  assert.equal(projectToggle.getAttribute("aria-expanded"), "false");
  assert.equal(projectToggle.getAttribute("aria-label"), "Expand project nav");

  projectToggle.click();

  assert.deepEqual(sessionButtonLabels(sessionList), [
    "Session 1",
    "Session 2",
    "Session 3",
  ]);
});

function loadRenderer({ sessions = [] } = {}) {
  const source = fs.readFileSync(
    path.join(__dirname, "../desktop/electron/renderer/renderer.js"),
    "utf8",
  );
  const context = {
    document: new FakeDocument(),
    Intl,
    window: {
      DOMPurify: {
        addHook() {},
        sanitize(markup) {
          return markup;
        },
      },
      localStorage: new FakeLocalStorage(),
      marked: {
        parse(sourceMarkdown) {
          return sourceMarkdown;
        },
        setOptions() {},
      },
      navProjectList: projectList,
      nav: {
        onBackendStatus() {},
        onSessionEvent() {},
        async listSessions() {
          return sessions;
        },
      },
    },
  };
  vm.createContext(context);
  vm.runInContext(source, context);
  return context;
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

function sessionButtonLabels(root) {
  return findAllByClass(root, "session-item").map((item) => item.textContent);
}

function findByClass(root, className) {
  return findAllByClass(root, className).at(0) ?? null;
}

function findAllByClass(root, className) {
  const matches = [];
  visit(root, (element) => {
    if (element.className.split(/\s+/).includes(className)) {
      matches.push(element);
    }
  });
  return matches;
}

function visit(element, callback) {
  callback(element);
  for (const child of element.children) {
    visit(child, callback);
  }
}

class FakeLocalStorage {
  constructor() {
    this.items = new Map();
  }

  getItem(key) {
    return this.items.get(key) ?? null;
  }

  setItem(key, value) {
    this.items.set(key, String(value));
  }
}

class FakeDocument {
  constructor() {
    this.nodes = new Map();
  }

  querySelector(selector) {
    if (!this.nodes.has(selector)) {
      this.nodes.set(selector, new FakeElement("div"));
    }
    return this.nodes.get(selector);
  }

  createElement(tagName) {
    return new FakeElement(tagName);
  }
}

class FakeElement {
  constructor(tagName) {
    this.tagName = tagName.toUpperCase();
    this.attributes = new Map();
    this.children = [];
    this.className = "";
    this.dataset = {};
    this.eventListeners = new Map();
    this.style = {};
    this.textContent = "";
    this.type = "";
    this.disabled = false;
    this.hidden = false;
  }

  addEventListener(type, handler) {
    this.eventListeners.set(type, handler);
  }

  append(...children) {
    this.children.push(...children);
  }

  click() {
    this.eventListeners.get("click")?.();
  }

  focus() {}

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  querySelectorAll(selector) {
    if (!selector.startsWith(".")) {
      return [];
    }
    return findAllByClass(this, selector.slice(1));
  }

  removeAttribute(name) {
    this.attributes.delete(name);
  }

  replaceChildren(...children) {
    this.children = children;
  }

  requestSubmit() {}

  scrollIntoView() {}

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
}
