import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterEach, describe, expect, test } from "vitest";
import { createControlPlane } from "../src/control-plane.js";
import { ModelCatalog } from "../src/model-catalog.js";
import type { BackendServices } from "../src/services.js";
import { SessionCatalog } from "../src/session-catalog.js";
import { StackStore } from "../src/stacks.js";
import type { ExecFileLike } from "../src/worktrees.js";

const tempDirs: string[] = [];

afterEach(async () => {
  await Promise.all(
    tempDirs.splice(0).map((dir) => rm(dir, { recursive: true, force: true })),
  );
});

describe("nav control plane", () => {
  test("creates, lists, resumes, finds latest, and deletes local sessions", async () => {
    const workspace = await tempDir("nav-workspace-");
    const services = await testServices({ defaultCwd: workspace });
    const app = createControlPlane(services);

    const created = await json<{ sessionId: string }>(
      await app.request("/sessions", { method: "POST" }),
    );

    expect(created.sessionId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );

    const listed = await json<{
      sessions: { sessionId: string; workspaceRoot: string | null }[];
    }>(await app.request("/sessions"));

    expect(listed.sessions).toMatchObject([
      {
        sessionId: created.sessionId,
        workspaceRoot: workspace,
      },
    ]);

    const latest = await json<{ sessionId: string | null }>(
      await app.request(
        `/sessions/latest?cwd=${encodeURIComponent(workspace)}`,
      ),
    );
    expect(latest.sessionId).toBe(created.sessionId);

    const resumed = await json<{ sessionId: string }>(
      await app.request(`/sessions/${created.sessionId}/resume`, {
        method: "POST",
      }),
    );
    expect(resumed.sessionId).toBe(created.sessionId);

    const deleted = await json<{ deleted: boolean }>(
      await app.request(`/sessions/${created.sessionId}`, {
        method: "DELETE",
      }),
    );
    expect(deleted.deleted).toBe(true);

    const afterDelete = await json<{ sessionId: string | null }>(
      await app.request(
        `/sessions/latest?cwd=${encodeURIComponent(workspace)}`,
      ),
    );
    expect(afterDelete.sessionId).toBeNull();
  });

  test("returns model shapes and persists per-session model choices", async () => {
    const workspace = await tempDir("nav-workspace-");
    const services = await testServices({ defaultCwd: workspace });
    const app = createControlPlane(services);
    const created = await json<{ sessionId: string }>(
      await app.request("/sessions", { method: "POST" }),
    );

    const models = await json<{
      models: { provider: string; model: string }[];
    }>(await app.request("/models"));
    expect(models.models).toContainEqual(
      expect.objectContaining({
        provider: "anthropic",
        model: "claude-sonnet-4-6",
      }),
    );

    const before = await json<{ provider: string; model: string }>(
      await app.request(`/sessions/${created.sessionId}/model`),
    );
    expect(before).toMatchObject({
      provider: "anthropic",
      model: "claude-sonnet-4-6",
    });

    const switched = await json<{
      modelInfo: { provider: string; model: string; thinking: string };
    }>(
      await app.request(`/sessions/${created.sessionId}/model`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          provider: "openai",
          model: "gpt-5",
          thinkingLevel: "high",
        }),
      }),
    );
    expect(switched.modelInfo).toMatchObject({
      provider: "openai",
      model: "gpt-5",
      thinking: "high",
    });

    const thinking = await json<{
      modelInfo: { provider: string; model: string; thinking: string };
    }>(
      await app.request(`/sessions/${created.sessionId}/thinking`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ thinkingLevel: "off" }),
      }),
    );
    expect(thinking.modelInfo).toMatchObject({
      provider: "openai",
      model: "gpt-5",
      thinking: "off",
    });
  });

  test("selects the offline mock model when smoke mode is enabled", () => {
    const models = new ModelCatalog({ env: { NAV_MOCK_MODEL: "1" } });

    expect(models.defaultSelection()).toMatchObject({
      provider: "nav-mock",
      model: "nav-smoke",
      thinkingLevel: "off",
    });
    expect(models.defaultModelInfo()).toMatchObject({
      label: "nav Offline Smoke Mock",
      provider: "nav-mock",
      model: "nav-smoke",
      thinking: "off",
      tokenUsage: {
        used: 0,
        contextWindow: 128_000,
      },
    });
    expect(models.list()).toContainEqual(
      expect.objectContaining({
        provider: "nav-mock",
        model: "nav-smoke",
        thinkingLevels: ["off"],
      }),
    );
  });

  test("creates tracked worktree sessions with the agent cwd pointed at the worktree", async () => {
    const workspace = await tempDir("nav-workspace-");
    const projectRoot = await tempDir("nav-project-");
    const dataDir = await tempDir("nav-data-");
    const calls: { file: string; args: string[]; cwd: string }[] = [];
    const execFile: ExecFileLike = async (file, args, options) => {
      calls.push({ file, args, cwd: options.cwd });
      if (args.join(" ") === "rev-parse --show-toplevel") {
        return { stdout: `${projectRoot}\n`, stderr: "" };
      }
      return { stdout: "", stderr: "" };
    };
    const services = await testServices({
      dataDir,
      defaultCwd: workspace,
      execFile,
    });
    const app = createControlPlane(services);

    const created = await json<{ sessionId: string }>(
      await app.request("/sessions", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ cwd: workspace, mode: "worktree" }),
      }),
    );
    const session = await services.catalog.get(created.sessionId);

    expect(session).toMatchObject({
      mode: "worktree",
      workspaceRoot: workspace,
      projectRoot,
      agentCwd: resolve(dataDir, "worktrees", created.sessionId),
      worktreePath: resolve(dataDir, "worktrees", created.sessionId),
    });
    expect(calls).toContainEqual({
      file: "git",
      args: [
        "worktree",
        "add",
        "--detach",
        resolve(dataDir, "worktrees", created.sessionId),
        "HEAD",
      ],
      cwd: projectRoot,
    });
  });

  test("stores sanitized stack rows from synthetic Flue turn observations", async () => {
    const workspace = await tempDir("nav-workspace-");
    const services = await testServices({ defaultCwd: workspace });
    const app = createControlPlane(services);
    const created = await json<{ sessionId: string }>(
      await app.request("/sessions", { method: "POST" }),
    );

    await services.stacks.recordObservation({
      type: "turn_request",
      instanceId: created.sessionId,
      operationId: "operation-1",
      turnId: "turn-1",
      timestamp: "2026-06-25T00:00:00.000Z",
      request: {
        providerName: "openai",
        url: "https://api.openai.example/v1/responses",
        model: "gpt-5",
        input: [{ role: "user", content: "hello" }],
      },
    });
    await services.stacks.recordObservation({
      type: "turn",
      instanceId: created.sessionId,
      operationId: "operation-1",
      turnId: "turn-1",
      durationMs: 42,
      isError: false,
      response: {
        output: [{ role: "assistant", content: "hi" }],
        usage: { inputTokens: 1, outputTokens: 1 },
      },
    });

    const stacks = await json<{
      stacks: {
        status: string;
        durationMs: number;
        request: { api: string; model: string; body: unknown };
        response: { body: unknown; tokenUsage: unknown };
      }[];
    }>(await app.request(`/sessions/${created.sessionId}/stacks`));

    expect(stacks.stacks).toHaveLength(1);
    expect(stacks.stacks[0]).toMatchObject({
      status: "completed",
      durationMs: 42,
      request: {
        api: "openai",
        model: "gpt-5",
        body: [{ role: "user", content: "hello" }],
      },
      response: {
        body: [{ role: "assistant", content: "hi" }],
        tokenUsage: { inputTokens: 1, outputTokens: 1 },
      },
    });

    const availability = await json<{ available: boolean }>(
      await app.request(`/sessions/${created.sessionId}/stacks/availability`),
    );
    expect(availability.available).toBe(true);
  });
});

async function testServices({
  defaultCwd,
  dataDir,
  execFile = async () => {
    throw new Error("git unavailable in test");
  },
}: {
  defaultCwd: string;
  dataDir?: string;
  execFile?: ExecFileLike;
}): Promise<BackendServices> {
  const resolvedDataDir = dataDir ?? (await tempDir("nav-data-"));
  const models = new ModelCatalog();

  return {
    models,
    catalog: new SessionCatalog({
      filePath: join(resolvedDataDir, "sessions.json"),
      defaultCwd,
      worktreeBaseDir: join(resolvedDataDir, "worktrees"),
      models,
      execFile,
    }),
    stacks: new StackStore({
      filePath: join(resolvedDataDir, "stacks.json"),
    }),
  };
}

async function tempDir(prefix: string): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), prefix));
  const resolved = await realpath(dir);
  tempDirs.push(resolved);
  return resolved;
}

async function json<T>(response: Response, status = 200): Promise<T> {
  expect(response.status).toBe(status);
  return (await response.json()) as T;
}
