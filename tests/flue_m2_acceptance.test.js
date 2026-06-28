import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { mkdir, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

const repoRoot = path.resolve(import.meta.dirname, "..");
const flueRoot = path.join(repoRoot, "packages", "flue");
const buildDir = mkdtempSync(path.join(tmpdir(), "nav-flue-test-build-"));

const compileFlue = () => {
  execFileSync(
    path.join(flueRoot, "node_modules", ".bin", "tsc"),
    [
      "--project",
      path.join(flueRoot, "tsconfig.json"),
      "--outDir",
      buildDir,
      "--noEmit",
      "false",
      "--declaration",
      "false",
      "--declarationMap",
      "false",
    ],
    { cwd: flueRoot, stdio: "inherit" },
  );
  writeFileSync(
    path.join(buildDir, "package.json"),
    JSON.stringify({ type: "module" }),
  );
  symlinkSync(
    path.join(flueRoot, "node_modules"),
    path.join(buildDir, "node_modules"),
  );
};

const importBuilt = (modulePath) =>
  import(pathToFileURL(path.join(buildDir, modulePath)).href);

const callProjectHandler = async (handler, { body, id, query = {} } = {}) =>
  await handler({
    req: {
      json: async () => body,
      param: (name) => (name === "id" ? id : undefined),
      query: (name) => query[name],
    },
    json: (payload, status = 200) => ({ body: payload, status }),
  });

const insertStoredSession = (db, sessionId) => {
  const storageKey = `agent-session:${JSON.stringify([
    sessionId,
    "default",
    "default",
  ])}`;

  db.prepare(
    `INSERT INTO flue_sessions (id, data)
     VALUES (?, ?)`,
  ).run(
    storageKey,
    JSON.stringify({
      createdAt: Date.now(),
      updatedAt: Date.now(),
    }),
  );
};

const git = (cwd, args) =>
  execFileSync("git", ["-C", cwd, ...args], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  }).trim();

const initRepo = async (root) => {
  await mkdir(root, { recursive: true });
  git(root, ["init"]);
  await writeFile(path.join(root, "README.md"), "hello\n");
  git(root, ["add", "-A"]);
  execFileSync(
    "git",
    [
      "-C",
      root,
      "-c",
      "user.email=nav@example.invalid",
      "-c",
      "user.name=Nav Test",
      "commit",
      "-m",
      "init",
    ],
    { stdio: "ignore" },
  );
};

compileFlue();

test.after(() => {
  rmSync(buildDir, { recursive: true, force: true });
});

test("validateGitContext canonicalizes subpaths and rejects symlink escapes", async () => {
  const { validateGitContext } = await importBuilt(
    ".flue/shared/git-context.js",
  );
  const temp = mkdtempSync(path.join(tmpdir(), "nav-git-context-"));
  const repo = path.join(temp, "repo");
  const outside = path.join(temp, "outside");

  try {
    await initRepo(repo);
    await mkdir(path.join(repo, "actual"), { recursive: true });
    await mkdir(outside);
    await symlink("actual", path.join(repo, "link-in"), "dir");
    await symlink(outside, path.join(repo, "link-out"), "dir");

    assert.deepEqual(validateGitContext(repo, "link-in"), {
      gitRoot: realpathSync(repo),
      subpath: "actual",
    });
    assert.throws(
      () => validateGitContext(repo, "link-out"),
      /project path must live inside its git root/,
    );
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

test("resolveGitContext gates non-git and no-HEAD projects", async () => {
  const { resolveGitContext } = await importBuilt(
    ".flue/shared/git-context.js",
  );
  const temp = mkdtempSync(path.join(tmpdir(), "nav-git-gate-"));
  const nonGit = path.join(temp, "non-git");
  const emptyRepo = path.join(temp, "empty-repo");

  try {
    await mkdir(nonGit);
    await mkdir(emptyRepo);
    git(emptyRepo, ["init"]);

    assert.deepEqual(resolveGitContext(nonGit), {
      ok: false,
      reason: "project is not a git repo",
    });
    assert.deepEqual(resolveGitContext(emptyRepo), {
      ok: false,
      reason: "project has no commits yet",
    });
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

test("delegate cwd resolution fails closed without request headers", async () => {
  const { resolveDelegateCwd } = await importBuilt(
    ".flue/shared/delegate-runtime.js",
  );

  assert.throws(
    () => resolveDelegateCwd("glm", "missing-context"),
    /Missing delegation context/,
  );
});

test("consult tools pass git context headers and repo-scoped worktree paths", async () => {
  const { makeConsult, makeConsultPanel } = await importBuilt(
    ".flue/shared/delegation.js",
  );
  const { agentWorktreePath } = await importBuilt(".flue/shared/worktrees.js");
  const previousFetch = globalThis.fetch;
  const previousPort = process.env.NAV_FLUE_PORT;
  const previousToken = process.env.NAV_DESKTOP_TOKEN;
  const gitCtx = {
    gitRoot: path.join(tmpdir(), "nav-header-repo"),
    subpath: "packages/example",
  };
  const calls = [];

  process.env.NAV_FLUE_PORT = "3583";
  process.env.NAV_DESKTOP_TOKEN = "test-token";
  globalThis.fetch = async (url, init) => {
    calls.push({ url, init });

    const agent = new URL(url).pathname.split("/").at(-2);

    return new Response(JSON.stringify({ result: agent }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  };

  try {
    const result = await makeConsult(gitCtx).run({
      input: { agent: "glm", task: "check this" },
      signal: undefined,
      emitData: () => {},
    });
    const delegationId = new URL(calls[0].url).pathname.split("/").at(-1);

    assert.equal(calls.length, 1);
    assert.equal(calls[0].init.headers["X-Nav-Repo-Root"], gitCtx.gitRoot);
    assert.equal(calls[0].init.headers["X-Nav-Subpath"], gitCtx.subpath);
    assert.equal(result.agent, "glm");
    assert.equal(result.answer, "glm");
    assert.equal(
      result.worktree,
      agentWorktreePath("glm", delegationId, gitCtx.gitRoot),
    );

    calls.length = 0;

    const panel = await makeConsultPanel(gitCtx).run({
      input: {
        agents: ["glm", "deepseek-pro", "deepseek-flash"],
        task: "check this",
      },
      signal: undefined,
      emitData: () => {},
    });

    assert.equal(calls.length, 3);
    assert.deepEqual(
      panel.results.map((item) => item.agent),
      ["glm", "deepseek-pro", "deepseek-flash"],
    );

    for (const call of calls) {
      const url = new URL(call.url);
      const parts = url.pathname.split("/");
      const agent = parts.at(-2);
      const id = parts.at(-1);

      assert.equal(call.init.headers["X-Nav-Repo-Root"], gitCtx.gitRoot);
      assert.equal(call.init.headers["X-Nav-Subpath"], gitCtx.subpath);
      assert.equal(
        panel.results.find((item) => item.agent === agent)?.worktree,
        agentWorktreePath(agent, id, gitCtx.gitRoot),
      );
    }
  } finally {
    globalThis.fetch = previousFetch;

    if (previousPort == null) {
      delete process.env.NAV_FLUE_PORT;
    } else {
      process.env.NAV_FLUE_PORT = previousPort;
    }

    if (previousToken == null) {
      delete process.env.NAV_DESKTOP_TOKEN;
    } else {
      process.env.NAV_DESKTOP_TOKEN = previousToken;
    }
  }
});

test("session title helpers enforce eligibility and short titles", async () => {
  const {
    buildTitlePrompt,
    hasTitleTranscriptExchange,
    isTitleSourceEligible,
    normalizeGeneratedTitle,
    TITLE_MODEL,
  } = await importBuilt(".flue/shared/session-title.js");
  const transcript = [
    { role: "user", text: "Can you debug the failing fleet worktree test?" },
    { role: "assistant", text: "I found the repo root mismatch." },
  ];

  assert.equal(TITLE_MODEL, "deepseek/deepseek-v4-flash");
  assert.equal(isTitleSourceEligible("first-message"), true);
  assert.equal(isTitleSourceEligible("imported"), true);
  assert.equal(isTitleSourceEligible("manual"), false);
  assert.equal(isTitleSourceEligible("llm"), false);
  assert.equal(hasTitleTranscriptExchange(transcript), true);
  assert.equal(
    normalizeGeneratedTitle(
      '"Debugging the Project Fleet Worktree Routing Failure"',
    ),
    "Debugging the Project Fleet Worktree Routing",
  );
  assert.match(buildTitlePrompt(transcript), /at most 6 words/);
});

test("request classifier helpers build prompts and normalize structured output", async () => {
  const {
    buildRequestClassifierPrompt,
    normalizeRequestClassification,
    REQUEST_CLASSIFIER_MODEL,
  } = await importBuilt(".flue/shared/request-classifier.js");

  const prompt = buildRequestClassifierPrompt({
    priorAssistant: "I can split this into a plan first.",
    text: "ok do it",
  });

  assert.equal(REQUEST_CLASSIFIER_MODEL, "deepseek/deepseek-v4-flash");
  assert.match(prompt, /Prior assistant turn:/);
  assert.match(prompt, /ok do it/);
  assert.deepEqual(
    normalizeRequestClassification({
      difficulty: "high",
      isPlanning: true,
    }),
    { difficulty: "high", isPlanning: true },
  );
  assert.equal(
    normalizeRequestClassification({
      difficulty: "extreme",
      isPlanning: true,
    }),
    null,
  );
  assert.equal(
    normalizeRequestClassification({
      difficulty: "low",
      isPlanning: "yes",
    }),
    null,
  );
});

test("project APIs relocate, restore, configure, and reorder projects", async () => {
  const temp = mkdtempSync(path.join(tmpdir(), "nav-project-relocate-"));
  const previousCwd = process.cwd();
  const previousWorkdir = process.env.NAV_CODEX_WORKDIR;

  try {
    const dataRoot = path.join(temp, "data");
    const defaultRoot = path.join(temp, "default");
    const originalRoot = path.join(temp, "original");
    const relocatedRoot = path.join(temp, "relocated");
    const duplicateRoot = path.join(temp, "duplicate");

    mkdirSync(dataRoot);
    mkdirSync(defaultRoot);
    mkdirSync(originalRoot);
    mkdirSync(relocatedRoot);
    mkdirSync(duplicateRoot);

    process.chdir(temp);
    process.env.NAV_CODEX_WORKDIR = defaultRoot;

    const {
      handleCreateNavProject,
      handleListNavProjects,
      handleReorderNavProjects,
      handleUpdateNavProject,
      resolveSessionProject,
    } = await importBuilt(".flue/shared/nav-projects.js");
    const { prepareOrchestratorTurn } = await importBuilt(
      ".flue/shared/orchestrator.js",
    );
    const { handleListNavSessions } = await importBuilt(
      ".flue/shared/nav-sessions.js",
    );
    const { getNavDb } = await importBuilt(".flue/shared/nav-db.js");
    await callProjectHandler(handleListNavSessions);

    const created = await callProjectHandler(handleCreateNavProject, {
      body: { name: "Moved App", path: originalRoot },
    });

    assert.equal(created.status, 201);
    assert.equal(created.body.project.name, "Moved App");

    rmSync(originalRoot, { recursive: true, force: true });

    const relocated = await callProjectHandler(handleUpdateNavProject, {
      body: { path: relocatedRoot },
      id: created.body.project.id,
    });

    assert.equal(relocated.status, 200);
    assert.equal(relocated.body.project.name, "Moved App");
    assert.equal(relocated.body.project.path, realpathSync(relocatedRoot));
    assert.equal(
      relocated.body.project.displayPath,
      path.normalize(relocatedRoot),
    );
    assert.equal(relocated.body.project.available, true);
    assert.equal(relocated.body.project.archived, false);

    const configured = await callProjectHandler(handleUpdateNavProject, {
      body: {
        autoApproveEdits: true,
        color: "blue",
        icon: "terminal",
        modelSpec: "deepseek/deepseek-v4-pro",
        orchestratorEnabled: true,
      },
      id: created.body.project.id,
    });

    assert.equal(configured.status, 200);
    assert.equal(configured.body.project.autoApproveEdits, true);
    assert.equal(configured.body.project.color, "blue");
    assert.equal(configured.body.project.icon, "terminal");
    assert.equal(configured.body.project.modelSpec, "deepseek/deepseek-v4-pro");
    assert.equal(configured.body.project.orchestratorEnabled, true);

    const sessionId = "018f0000-0000-7000-8000-000000000001";
    getNavDb()
      .prepare(
        `INSERT INTO nav_sessions (
          id,
          agent_name,
          title,
          title_source,
          pinned,
          archived,
          project_id,
          created_at
         )
         VALUES (?, 'nav', NULL, 'first-message', 0, 0, ?, ?)`,
      )
      .run(sessionId, created.body.project.id, Date.now());
    insertStoredSession(getNavDb(), sessionId);

    const sessionProject = resolveSessionProject(sessionId);

    assert.equal(sessionProject.autoApproveEdits, true);
    assert.equal(sessionProject.modelSpec, "deepseek/deepseek-v4-pro");
    assert.equal(sessionProject.orchestratorEnabled, true);

    const previousFetch = globalThis.fetch;
    const previousPort = process.env.NAV_FLUE_PORT;
    const previousToken = process.env.NAV_DESKTOP_TOKEN;
    const calls = [];
    const nextClassifications = [
      { difficulty: "medium", isPlanning: false },
      { difficulty: "low", isPlanning: false },
      { difficulty: "high", isPlanning: false },
    ];

    process.env.NAV_FLUE_PORT = "3583";
    process.env.NAV_DESKTOP_TOKEN = "test-token";
    globalThis.fetch = async (url, init) => {
      calls.push({ init, url });

      const parsedUrl = new URL(url);

      if (parsedUrl.pathname === "/api/workflows/request-classifier") {
        return new Response(
          JSON.stringify({ result: nextClassifications.shift() }),
          {
            status: 200,
            headers: { "content-type": "application/json" },
          },
        );
      }

      const agent = parsedUrl.pathname.split("/").at(-2);

      if (agent === "deepseek-pro" && nextClassifications.length === 0) {
        return new Response("delegate failed", { status: 500 });
      }

      return new Response(JSON.stringify({ result: `${agent} answer` }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    };

    try {
      const gitCtx = { gitRoot: relocatedRoot, subpath: "" };
      const firstOrchestrated = await prepareOrchestratorTurn({
        git: gitCtx,
        message: "Implement a medium sized feature",
        project: sessionProject,
        sessionId,
      });

      assert.equal(firstOrchestrated.mode, "panel");
      assert.equal(firstOrchestrated.status, "complete");
      assert.equal(firstOrchestrated.active, true);
      assert.equal(firstOrchestrated.difficulty, "medium");
      assert.deepEqual(
        firstOrchestrated.delegateResults.map((result) => result.agent).sort(),
        ["deepseek-pro", "glm"],
      );

      const firstTurnRows = getNavDb()
        .prepare(
          "SELECT difficulty, mode, status FROM nav_orchestrator_turns WHERE session_id = ?",
        )
        .all(sessionId);

      assert.equal(firstTurnRows.length, 1);
      assert.equal(firstTurnRows[0].difficulty, "medium");
      assert.equal(firstTurnRows[0].mode, "panel");
      assert.equal(firstTurnRows[0].status, "complete");

      const lowFollowUp = await prepareOrchestratorTurn({
        git: gitCtx,
        message: "also rename the helper",
        project: sessionProject,
        sessionId,
      });

      assert.equal(lowFollowUp.mode, "direct");
      assert.equal(lowFollowUp.active, true);
      assert.equal(lowFollowUp.difficulty, "low");

      const partial = await prepareOrchestratorTurn({
        git: gitCtx,
        message: "now do the harder follow-up",
        project: sessionProject,
        sessionId,
      });

      assert.equal(partial.mode, "panel");
      assert.equal(partial.status, "partial");
      assert.equal(partial.difficulty, "high");
      assert.equal(
        partial.delegateResults.some(
          (result) =>
            result.agent === "deepseek-pro" && result.status === "failed",
        ),
        true,
      );

      const classifyCalls = calls.filter(
        (call) =>
          new URL(call.url).pathname === "/api/workflows/request-classifier",
      );
      const delegateCalls = calls.filter((call) =>
        new URL(call.url).pathname.startsWith("/api/agents/"),
      );

      assert.equal(classifyCalls.length, 3);
      assert.equal(delegateCalls.length, 4);
    } finally {
      globalThis.fetch = previousFetch;

      if (previousPort == null) {
        delete process.env.NAV_FLUE_PORT;
      } else {
        process.env.NAV_FLUE_PORT = previousPort;
      }

      if (previousToken == null) {
        delete process.env.NAV_DESKTOP_TOKEN;
      } else {
        process.env.NAV_DESKTOP_TOKEN = previousToken;
      }
    }

    const duplicate = await callProjectHandler(handleCreateNavProject, {
      body: { path: duplicateRoot },
    });

    assert.equal(duplicate.status, 201);

    const rejected = await callProjectHandler(handleUpdateNavProject, {
      body: { path: duplicateRoot },
      id: created.body.project.id,
    });

    assert.equal(rejected.status, 409);
    assert.equal(rejected.body.error, "project_path_exists");

    const archived = await callProjectHandler(handleUpdateNavProject, {
      body: { archived: true },
      id: created.body.project.id,
    });

    assert.equal(archived.status, 200);
    assert.equal(archived.body.project.archived, true);

    const activeSessions = await callProjectHandler(handleListNavSessions);
    assert.equal(
      activeSessions.body.sessions.some((session) => session.id === sessionId),
      false,
    );

    const activeList = await callProjectHandler(handleListNavProjects);
    assert.equal(
      activeList.body.projects.some(
        (project) => project.id === created.body.project.id,
      ),
      false,
    );

    const archivedList = await callProjectHandler(handleListNavProjects, {
      query: { archived: "true" },
    });
    assert.equal(
      archivedList.body.projects.some(
        (project) => project.id === created.body.project.id && project.archived,
      ),
      true,
    );

    const restored = await callProjectHandler(handleUpdateNavProject, {
      body: { archived: false },
      id: created.body.project.id,
    });

    assert.equal(restored.status, 200);
    assert.equal(restored.body.project.archived, false);

    const beforeReorder = await callProjectHandler(handleListNavProjects);
    const projectIds = beforeReorder.body.projects.map((project) => project.id);
    const reversedProjectIds = [...projectIds].reverse();
    const reordered = await callProjectHandler(handleReorderNavProjects, {
      body: { projectIds: reversedProjectIds },
    });

    assert.equal(reordered.status, 200);

    const afterReorder = await callProjectHandler(handleListNavProjects);
    assert.deepEqual(
      afterReorder.body.projects.map((project) => project.id),
      reversedProjectIds,
    );
  } finally {
    process.chdir(previousCwd);

    if (previousWorkdir == null) {
      delete process.env.NAV_CODEX_WORKDIR;
    } else {
      process.env.NAV_CODEX_WORKDIR = previousWorkdir;
    }

    rmSync(temp, { recursive: true, force: true });
  }
});
