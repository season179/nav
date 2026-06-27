import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
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
