const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const Module = require("node:module");
const { test } = require("node:test");

// The reference validators that the unit tests already pin behavior against.
const rv = require("../desktop/electron/out/request-validation.cjs");

// preload.cts runs sandboxed and so inlines its own copy of the boundary
// validation instead of importing request-validation.cts (see the comment at the
// top of preload.cts). These tests load the *compiled preload itself* with a
// stubbed `electron` module so two gaps are covered:
//   1. Drift — the value preload forwards over IPC must match what
//      request-validation produces for the same input. If the inlined copy ever
//      diverges, the cross-checks below fail.
//   2. Reachability — preload.cjs is loaded by path from main.cts, so import
//      graph tooling sees it as unreachable. Actually requiring it here keeps it
//      exercised, and the final test pins the main.cts -> preload.cjs wiring.

type NavApi = Record<string, (...args: unknown[]) => unknown>;
type InvokeCall = { channel: string; payload: unknown };

// Load the compiled preload with `electron` stubbed, returning the exposed `nav`
// API plus a recorder for the last ipcRenderer.invoke call.
function loadPreload(): {
  nav: NavApi;
  lastCall: () => InvokeCall | undefined;
} {
  const calls: InvokeCall[] = [];
  let exposedApi: NavApi | null = null;
  const fakeElectron = {
    contextBridge: {
      exposeInMainWorld(_key: string, api: NavApi) {
        exposedApi = api;
      },
    },
    ipcRenderer: {
      invoke(channel: string, payload?: unknown) {
        calls.push({ channel, payload });
        return Promise.resolve(undefined);
      },
      on() {},
      removeListener() {},
    },
  };

  const preloadPath = require.resolve("../desktop/electron/out/preload.cjs");
  // Force a fresh evaluation so the stub is what preload sees.
  delete require.cache[preloadPath];
  // Module._load is internal and untyped; patching it is the supported way to
  // intercept preload's `require("electron")` in a plain Node test process.
  const loader = Module as unknown as {
    _load: (request: string, parent: unknown, isMain?: boolean) => unknown;
  };
  const originalLoad = loader._load;
  loader._load = function patchedLoad(
    request: string,
    parent: unknown,
    isMain?: boolean,
  ) {
    if (request === "electron") {
      return fakeElectron;
    }
    return originalLoad.call(this, request, parent, isMain);
  };
  try {
    require(preloadPath);
  } finally {
    loader._load = originalLoad;
    delete require.cache[preloadPath];
  }

  if (!exposedApi) {
    throw new Error("preload did not expose the nav API");
  }
  return {
    nav: exposedApi,
    lastCall: () => calls[calls.length - 1],
  };
}

test("preload exposes the expected nav API surface", () => {
  const { nav } = loadPreload();
  const methods = [
    "onBackendStatus",
    "onSessionEvent",
    "sessionSendMessage",
    "sessionStop",
    "listSessions",
    "createProject",
    "modelInfo",
    "modelList",
    "switchModel",
    "switchThinking",
    "sessionStacks",
    "sessionStackAvailability",
    "switchSession",
    "newSession",
    "getSessionMode",
    "setSessionMode",
  ];
  for (const name of methods) {
    assert.equal(typeof nav[name], "function", `nav.${name} should exist`);
  }
});

// Each entry exercises one exposed method against request-validation, asserting
// the IPC payload preload builds matches the reference validators byte for byte.
test("preload forwards the same normalized payload as request-validation", () => {
  const { nav, lastCall } = loadPreload();

  nav.sessionSendMessage("  s-1  ", "  hello  ");
  assert.deepEqual(lastCall(), {
    channel: "nav:send-message",
    payload: {
      sessionId: rv.normalizeSessionId("  s-1  "),
      text: rv.normalizeMessageText("  hello  "),
    },
  });

  nav.sessionStop("  s-2  ");
  assert.deepEqual(lastCall(), {
    channel: "nav:stop",
    payload: rv.normalizeSessionId("  s-2  "),
  });

  nav.switchModel("  s-m  ", "  openai  ", "  gpt-5.1  ", " high ");
  assert.deepEqual(lastCall(), {
    channel: "nav:switch-model",
    payload: {
      sessionId: rv.normalizeSessionId("  s-m  "),
      provider: rv.normalizeModelProvider("  openai  "),
      model: rv.normalizeModelId("  gpt-5.1  "),
      thinkingLevel: rv.normalizeThinkingLevel(" high "),
    },
  });

  // Omitted thinking level must drop the field, never send a default.
  nav.switchModel("s-m", "openai", "gpt-5.1", null);
  assert.deepEqual(lastCall(), {
    channel: "nav:switch-model",
    payload: { sessionId: "s-m", provider: "openai", model: "gpt-5.1" },
  });

  nav.switchThinking("  s-t  ", " medium ");
  assert.deepEqual(lastCall(), {
    channel: "nav:switch-thinking",
    payload: {
      sessionId: rv.normalizeSessionId("  s-t  "),
      thinkingLevel: rv.normalizeThinkingLevel(" medium "),
    },
  });

  nav.switchSession("  s-3  ");
  assert.deepEqual(lastCall(), {
    channel: "nav:switch-session",
    payload: rv.normalizeSessionId("  s-3  "),
  });

  nav.createProject("worktree");
  assert.deepEqual(lastCall(), {
    channel: "nav:create-project",
    payload: { mode: rv.normalizeOptionalSessionMode("worktree") },
  });

  nav.newSession("  /tmp/project  ", "local");
  assert.deepEqual(lastCall(), {
    channel: "nav:new-session",
    payload: {
      cwd: rv.normalizeOptionalWorkspaceRoot("  /tmp/project  "),
      mode: rv.normalizeOptionalSessionMode("local"),
    },
  });

  nav.setSessionMode("  worktree  ");
  assert.deepEqual(lastCall(), {
    channel: "nav:set-session-mode",
    payload: rv.normalizeSessionMode("  worktree  "),
  });
});

test("preload maps an omitted session id to undefined like the reference", () => {
  const { nav, lastCall } = loadPreload();

  nav.modelInfo(null);
  assert.deepEqual(lastCall(), {
    channel: "nav:model-info",
    payload: undefined,
  });

  nav.sessionStacks(null);
  assert.deepEqual(lastCall(), {
    channel: "nav:session-stacks",
    payload: undefined,
  });

  nav.sessionStackAvailability("  s-9  ");
  assert.deepEqual(lastCall(), {
    channel: "nav:session-stack-availability",
    payload: rv.normalizeSessionId("  s-9  "),
  });
});

// Bad inputs must be rejected at the boundary, and rejected the same way the
// reference validators reject them — proving the inlined copy stays in lockstep.
test("preload rejects the same invalid inputs as request-validation", () => {
  const { nav } = loadPreload();

  assert.throws(() => nav.sessionSendMessage(42, "hi"), TypeError);
  assert.throws(() => rv.normalizeSessionId(42), TypeError);

  assert.throws(() => nav.sessionSendMessage("s", "   "));
  assert.throws(() => rv.normalizeMessageText("   "));

  assert.throws(() => nav.switchThinking("s", "max"));
  assert.throws(() => rv.normalizeThinkingLevel("max"));

  assert.throws(() => nav.createProject("remote"));
  assert.throws(() => rv.normalizeOptionalSessionMode("remote"));

  assert.throws(() => nav.setSessionMode("remote"));
  assert.throws(() => rv.normalizeSessionMode("remote"));

  assert.throws(() => nav.switchSession("   "));
  assert.throws(() => rv.normalizeSessionId("   "));
});

test("preload subscribe helpers reject non-function listeners", () => {
  const { nav } = loadPreload();
  assert.throws(() => nav.onBackendStatus(undefined), TypeError);
  assert.throws(() => nav.onSessionEvent(42), TypeError);
  // A valid listener returns an unsubscribe function.
  assert.equal(typeof nav.onBackendStatus(() => {}), "function");
});

// preload.cjs is loaded by path from main.cts, not imported, so static tooling
// cannot see the link. Pin it so a rename or "dead code" removal of either side
// trips this test instead of silently breaking the renderer bridge.
test("main.cts wires the runtime-loaded preload entry by path", () => {
  const mainSource = fs.readFileSync(
    path.join(__dirname, "..", "desktop", "electron", "main.cts"),
    "utf8",
  );
  assert.match(mainSource, /preload\.cjs/);
});
