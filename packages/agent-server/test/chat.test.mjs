import assert from "node:assert/strict";
import { test } from "node:test";

import { handleAgentRequest } from "../dist/server.js";

const runner = {
  async *run(prompt) {
    yield { delta: `hello ${prompt}`, type: "text-delta" };
  },
};

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
    new Request("http://127.0.0.1:3583/api/chat", {
      body: JSON.stringify({
        messages: [
          {
            id: "user-1",
            parts: [{ text: "Season", type: "text" }],
            role: "user",
          },
        ],
      }),
      headers: { "content-type": "application/json" },
      method: "POST",
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
