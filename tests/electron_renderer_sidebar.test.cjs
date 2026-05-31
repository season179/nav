const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");

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

  const showMore = findByClass(collapsed, "project-show-more");
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
  assert.equal(findByClass(expanded, "project-show-more"), null);
});

test("project sidebar omits show more for five sessions", () => {
  const renderer = loadRenderer();
  const project = projectWithSessions(5);

  const list = renderer.renderProjectSessions(project);
  assert.equal(sessionButtonLabels(list).length, 5);
  assert.equal(findByClass(list, "project-show-more"), null);
});

function loadRenderer() {
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
      marked: {
        parse(sourceMarkdown) {
          return sourceMarkdown;
        },
        setOptions() {},
      },
      nav: {
        onBackendStatus() {},
        onSessionEvent() {},
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
