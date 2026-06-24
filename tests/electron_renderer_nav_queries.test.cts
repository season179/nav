import type { NavApi } from "../desktop/electron/renderer/src/types.ts";

const assert = require("node:assert/strict");
const { test } = require("node:test");

test("nav query helpers use the preload API surface", async () => {
  const {
    fetchModelOptions,
    fetchNavSessions,
    fetchSessionStacks,
    navQueryKeys,
  } = await loadNavQueries();
  const nav = fakeNavApi();

  assert.deepEqual(await fetchNavSessions(nav), [
    {
      sessionId: "s-1",
      title: "Thread",
      workspaceRoot: "/repo",
      projectRoot: "/repo",
      updatedAt: 42,
    },
  ]);
  assert.deepEqual(await fetchModelOptions(nav), [
    {
      provider: "openai",
      model: "gpt-5.1",
      label: "GPT-5.1",
      thinkingLevels: ["off", "low"],
    },
  ]);
  assert.deepEqual(await fetchSessionStacks("s-1", nav), {
    stacks: [
      {
        id: "stack-1",
        runId: "run-1",
        sequence: 0,
        status: "ok",
        startedAtMs: 10,
        durationMs: 5,
      },
    ],
  });

  assert.deepEqual(navQueryKeys.sessions(), ["nav", "sessions"]);
  assert.deepEqual(navQueryKeys.modelOptions(), ["nav", "models"]);
  assert.deepEqual(navQueryKeys.stacks("s-1"), ["nav", "stacks", "s-1"]);
});

test("stacks query is disabled without an active session", async () => {
  const { sessionStacksQueryOptions } = await loadNavQueries();

  const options = sessionStacksQueryOptions(null);

  assert.equal(options.enabled, false);
  assert.deepEqual(options.queryKey, ["nav", "stacks", "none"]);
});

function loadNavQueries() {
  return import("../desktop/electron/renderer/src/lib/nav-queries.ts");
}

function fakeNavApi(): NavApi {
  return {
    onBackendStatus: () => () => {},
    onSessionEvent: () => () => {},
    sessionSendMessage: async () => {},
    sessionStop: async () => false,
    listSessions: async () => [
      {
        sessionId: "s-1",
        title: "Thread",
        workspaceRoot: "/repo",
        projectRoot: "/repo",
        updatedAt: 42,
      },
    ],
    createProject: async () => "s-1",
    modelInfo: async () => ({
      label: "GPT-5.1",
      provider: "openai",
      model: "gpt-5.1",
      thinkingLevels: ["off", "low"],
    }),
    modelList: async () => [
      {
        provider: "openai",
        model: "gpt-5.1",
        label: "GPT-5.1",
        thinkingLevels: ["off", "low"],
      },
    ],
    switchModel: async () => ({
      label: "GPT-5.1",
      provider: "openai",
      model: "gpt-5.1",
      thinkingLevels: ["off", "low"],
    }),
    switchThinking: async () => ({
      label: "GPT-5.1",
      provider: "openai",
      model: "gpt-5.1",
      thinking: "low",
      thinkingLevels: ["off", "low"],
    }),
    sessionStacks: async () => ({
      stacks: [
        {
          id: "stack-1",
          runId: "run-1",
          sequence: 0,
          status: "ok",
          startedAtMs: 10,
          durationMs: 5,
        },
      ],
    }),
    sessionStackAvailability: async () => ({ available: true }),
    switchSession: async () => {},
    newSession: async () => "s-1",
    getSessionMode: async () => "local",
    setSessionMode: async () => {},
  };
}
