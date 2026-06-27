import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import { toPiModelReference } from "../dist/config.js";
import {
  createRequestAbortSignal,
  handleAgentRequest,
} from "../dist/server.js";

const chunk = (value) => `data: ${JSON.stringify(value)}\n\n`;

const createRunnerResponse = (text) =>
  new Response(
    [
      chunk({ id: "test-response", type: "text-start" }),
      chunk({ delta: text, id: "test-response", type: "text-delta" }),
      chunk({ id: "test-response", type: "text-end" }),
      "data: [DONE]\n\n",
    ].join(""),
    {
      headers: { "content-type": "text/event-stream; charset=utf-8" },
    },
  );

const getTextContent = (message) => {
  if (typeof message.content === "string") {
    return message.content;
  }

  if (!Array.isArray(message.content)) {
    return "";
  }

  return message.content
    .map((part) => (part.type === "text" ? part.text : ""))
    .join("");
};

const runner = {
  createResponse(messages) {
    const latestMessage = messages.findLast(
      (message) => message.role === "user",
    );
    return createRunnerResponse(`hello ${getTextContent(latestMessage)}`);
  },
};

const createChatRequest = (body, init = {}) =>
  new Request("http://127.0.0.1:3583/api/chat", {
    body: JSON.stringify(body),
    headers: { "content-type": "application/json" },
    method: "POST",
    ...init,
  });

test("health reports service identity", async () => {
  const response = await handleAgentRequest(
    new Request("http://127.0.0.1:3583/health"),
    runner,
  );
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), {
    ok: true,
    service: "@nav/agent-server",
  });
});

test("chat streams a UI message response", async () => {
  const response = await handleAgentRequest(
    createChatRequest({
      messages: [
        {
          id: "user-1",
          parts: [{ text: "Season", type: "text" }],
          role: "user",
        },
      ],
    }),
    runner,
  );

  assert.equal(response.status, 200);
  assert.match(
    response.headers.get("content-type") ?? "",
    /text\/event-stream/,
  );
  assert.match(await response.text(), /hello Season/);
});

test("chat passes text history as model messages", async () => {
  let capturedMessages = [];
  const response = await handleAgentRequest(
    createChatRequest({
      messages: [
        {
          id: "user-1",
          parts: [{ text: "What is this repo?", type: "text" }],
          role: "user",
        },
        {
          id: "assistant-1",
          parts: [{ text: "It is Nav.", type: "text" }],
          role: "assistant",
        },
        {
          id: "user-2",
          parts: [{ text: "What tests should I run?", type: "text" }],
          role: "user",
        },
      ],
    }),
    {
      createResponse(messages) {
        capturedMessages = messages;
        return createRunnerResponse("ok");
      },
    },
  );

  await response.text();

  assert.equal(capturedMessages.length, 1);
  assert.equal(capturedMessages[0].role, "user");
  assert.equal(
    getTextContent(capturedMessages[0]),
    [
      "Conversation so far:",
      "user: What is this repo?",
      "assistant: It is Nav.",
      "",
      "Current user request:",
      "What tests should I run?",
    ].join("\n"),
  );
});

test("chat passes the request abort signal to the runner", async () => {
  const controller = new AbortController();
  const request = createChatRequest(
    {
      messages: [
        {
          id: "user-1",
          parts: [{ text: "stop test", type: "text" }],
          role: "user",
        },
      ],
    },
    { signal: controller.signal },
  );
  let capturedSignal;

  await handleAgentRequest(request, {
    createResponse(_messages, options) {
      capturedSignal = options?.signal;
      return createRunnerResponse("ok");
    },
  });

  assert.equal(capturedSignal, request.signal);
});

test("Pi model references include the configured provider", () => {
  assert.equal(toPiModelReference("zai", "glm-5.2"), "zai/glm-5.2");
  assert.equal(toPiModelReference("zai", "zai/glm-5.2"), "zai/glm-5.2");
  assert.equal(
    toPiModelReference("openrouter", "zai/glm-5.2"),
    "openrouter/zai/glm-5.2",
  );
  assert.equal(toPiModelReference(undefined, "glm-5.2"), "glm-5.2");
});

test("harness-pi patch prefers provider-qualified model references", async () => {
  const resolverSource = await readFile(
    new URL(
      "../node_modules/@ai-sdk/harness-pi/src/pi-model-resolver.ts",
      import.meta.url,
    ),
    "utf8",
  );

  assert.match(resolverSource, /matchesProviderQualifiedModel/);
  assert.match(resolverSource, /\$\{m\.provider\}\/\$\{m\.id\}/);
});

test("harness-pi patch maps Pi promise filesystem calls through the workspace VFS", async () => {
  const vfsSource = await readFile(
    new URL(
      "../node_modules/@ai-sdk/harness-pi/src/pi-workspace-vfs.ts",
      import.meta.url,
    ),
    "utf8",
  );

  assert.match(vfsSource, /lstat: fs\.promises\.lstat/);
  assert.match(vfsSource, /readdir: fs\.promises\.readdir/);
  assert.match(vfsSource, /realpath: fs\.promises\.realpath/);
  assert.match(vfsSource, /mutableFsPromises\.lstat =/);
  assert.match(vfsSource, /mutableFsPromises\.readdir =/);
  assert.match(vfsSource, /mapRealpathResult/);
});

test("harness-pi patch suspends the workspace VFS while syncing from just-bash", async () => {
  const sessionSource = await readFile(
    new URL(
      "../node_modules/@ai-sdk/harness-pi/src/pi-session.ts",
      import.meta.url,
    ),
    "utf8",
  );

  assert.match(sessionSource, /syncSandboxWorkspaceIntoHostMirror/);
  assert.match(sessionSource, /workspaceVfs\.unmount\(\)/);
  assert.match(sessionSource, /finally\s*\{/);
  assert.match(
    sessionSource,
    /workspaceVfs\.mount\(hostWorkDir, sessionWorkDir\)/,
  );
  assert.match(
    sessionSource,
    /await syncSandboxWorkspaceIntoHostMirror\(\);\s*await resourceLoader\.reload\(\);/,
  );
});

test("malformed chat request returns 400", async () => {
  const response = await handleAgentRequest(
    new Request("http://127.0.0.1:3583/api/chat", {
      body: "{",
      headers: { "content-type": "application/json" },
      method: "POST",
    }),
    runner,
  );

  assert.equal(response.status, 400);
});

test("missing messages returns 400", async () => {
  const response = await handleAgentRequest(createChatRequest({}), runner);

  assert.equal(response.status, 400);
});

test("chat with no user message returns 400", async () => {
  const response = await handleAgentRequest(
    createChatRequest({
      messages: [
        {
          id: "assistant-1",
          parts: [{ text: "I am waiting.", type: "text" }],
          role: "assistant",
        },
      ],
    }),
    runner,
  );

  assert.equal(response.status, 400);
});

test("request abort signal tracks premature response close", () => {
  const incomingRequest = new EventEmitter();
  const serverResponse = new EventEmitter();
  serverResponse.writableEnded = false;

  const signal = createRequestAbortSignal(incomingRequest, serverResponse);

  assert.equal(signal.aborted, false);
  serverResponse.emit("close");
  assert.equal(signal.aborted, true);
});

test("request abort signal ignores normal response completion", () => {
  const incomingRequest = new EventEmitter();
  const serverResponse = new EventEmitter();
  serverResponse.writableEnded = true;

  const signal = createRequestAbortSignal(incomingRequest, serverResponse);

  serverResponse.emit("finish");
  serverResponse.emit("close");
  assert.equal(signal.aborted, false);
});
