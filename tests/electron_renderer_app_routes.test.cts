const assert = require("node:assert/strict");
const { test } = require("node:test");

test("nav route helpers build Electron-safe hash paths", async () => {
  const { chatPath, sessionChatPath, sessionStacksPath, settingsPath } =
    await loadAppRoutes();

  assert.equal(chatPath(), "/chat");
  assert.equal(sessionChatPath("session 1"), "/sessions/session%201");
  assert.equal(sessionStacksPath("session 1"), "/sessions/session%201/stacks");
  assert.equal(settingsPath(), "/settings");
});

test("nav route parser deep-links sessions and thread views", async () => {
  const { parseNavPathname } = await loadAppRoutes();

  assert.deepEqual(parseNavPathname("/chat"), {
    canonicalPath: "/chat",
    known: true,
    sessionId: null,
    view: "chat",
  });
  assert.deepEqual(parseNavPathname("/sessions/session%201"), {
    canonicalPath: "/sessions/session%201",
    known: true,
    sessionId: "session 1",
    view: "chat",
  });
  assert.deepEqual(parseNavPathname("/sessions/session%201/stacks"), {
    canonicalPath: "/sessions/session%201/stacks",
    known: true,
    sessionId: "session 1",
    view: "stacks",
  });
});

test("unknown nav routes fall back to chat", async () => {
  const { parseNavPathname } = await loadAppRoutes();

  assert.deepEqual(parseNavPathname("/"), {
    canonicalPath: "/chat",
    known: false,
    sessionId: null,
    view: "chat",
  });
  assert.deepEqual(parseNavPathname("/missing"), {
    canonicalPath: "/chat",
    known: false,
    sessionId: null,
    view: "chat",
  });
});

function loadAppRoutes() {
  return import("../desktop/electron/renderer/src/lib/app-routes.ts");
}
