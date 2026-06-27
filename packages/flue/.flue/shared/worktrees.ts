import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { resolveGitRoot } from "./git-context.js";
import { listNavProjectPathsForWorktreePrune } from "./nav-projects.js";

const WORKTREE_BASE = path.join(tmpdir(), "nav-worktrees");

function worktreeRoot(repoRoot: string): string {
  const repoHash = createHash("sha256")
    .update(repoRoot)
    .digest("hex")
    .slice(0, 12);

  return path.join(WORKTREE_BASE, repoHash);
}

export function agentWorktreePath(
  agent: string,
  instanceId: string,
  repoRoot: string,
): string {
  return path.join(worktreeRoot(repoRoot), agent, instanceId);
}

function createWorkspaceSnapshot(repoRoot: string): string {
  const tempDir = mkdtempSync(path.join(tmpdir(), "nav-snapshot-index-"));
  const env = {
    ...process.env,
    GIT_AUTHOR_EMAIL: "nav@example.invalid",
    GIT_AUTHOR_NAME: "Nav Delegation",
    GIT_COMMITTER_EMAIL: "nav@example.invalid",
    GIT_COMMITTER_NAME: "Nav Delegation",
    GIT_INDEX_FILE: path.join(tempDir, "index"),
  };

  try {
    execFileSync("git", ["-C", repoRoot, "read-tree", "HEAD"], {
      env,
      stdio: "ignore",
    });
    execFileSync("git", ["-C", repoRoot, "add", "-A"], {
      env,
      stdio: "ignore",
    });

    const tree = execFileSync("git", ["-C", repoRoot, "write-tree"], {
      encoding: "utf8",
      env,
    }).trim();

    return execFileSync(
      "git",
      [
        "-C",
        repoRoot,
        "commit-tree",
        tree,
        "-p",
        "HEAD",
        "-m",
        "nav delegation snapshot",
      ],
      {
        encoding: "utf8",
        env,
      },
    ).trim();
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

export function createAgentWorktree(
  agent: string,
  instanceId: string,
  repoRoot: string,
): string {
  const worktree = agentWorktreePath(agent, instanceId, repoRoot);

  if (existsSync(worktree)) {
    return worktree;
  }

  const snapshot = createWorkspaceSnapshot(repoRoot);

  mkdirSync(path.dirname(worktree), { recursive: true });
  execFileSync(
    "git",
    ["-C", repoRoot, "worktree", "add", "--detach", worktree, snapshot],
    { stdio: "ignore" },
  );

  return worktree;
}

export function pruneAgentWorktrees(): void {
  const gitRoots = new Set<string>();

  for (const projectPath of listNavProjectPathsForWorktreePrune()) {
    const root = resolveGitRoot(projectPath);

    if (root.ok) {
      gitRoots.add(root.gitRoot);
    }
  }

  rmSync(WORKTREE_BASE, { recursive: true, force: true });

  for (const gitRoot of gitRoots) {
    execFileSync("git", ["-C", gitRoot, "worktree", "prune"], {
      stdio: "ignore",
    });
  }
}
