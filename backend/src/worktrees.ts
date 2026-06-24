import { execFile as execFileCallback } from "node:child_process";
import { mkdir, realpath, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";

export type SessionMode = "local" | "worktree";

export type ExecFileLike = (
  file: string,
  args: string[],
  options: { cwd: string },
) => Promise<{ stdout: string; stderr: string }>;

export type PreparedWorkspace = {
  workspaceRoot: string;
  projectRoot: string;
  agentCwd: string;
  worktreePath: string | null;
};

export const defaultExecFile: ExecFileLike = (file, args, options) =>
  new Promise((resolvePromise, reject) => {
    execFileCallback(
      file,
      args,
      { cwd: options.cwd, encoding: "utf8" },
      (error, stdout, stderr) => {
        if (error) {
          reject(error);
          return;
        }

        resolvePromise({ stdout, stderr });
      },
    );
  });

export async function prepareWorkspace({
  cwd,
  mode,
  sessionId,
  worktreeBaseDir,
  execFile = defaultExecFile,
}: {
  cwd: string;
  mode: SessionMode;
  sessionId: string;
  worktreeBaseDir: string;
  execFile?: ExecFileLike;
}): Promise<PreparedWorkspace> {
  const workspaceRoot = await realpath(cwd);
  const gitProjectRoot = await resolveGitProjectRoot(workspaceRoot, execFile);
  const projectRoot = gitProjectRoot ?? workspaceRoot;

  if (mode === "local") {
    return {
      workspaceRoot,
      projectRoot,
      agentCwd: workspaceRoot,
      worktreePath: null,
    };
  }

  if (!gitProjectRoot) {
    throw new Error(
      `worktree mode requires a git repository: ${workspaceRoot}`,
    );
  }

  const worktreePath = resolve(worktreeBaseDir, sessionId);
  await mkdir(dirname(worktreePath), { recursive: true });
  await execFile("git", ["worktree", "add", "--detach", worktreePath, "HEAD"], {
    cwd: gitProjectRoot,
  });

  return {
    workspaceRoot,
    projectRoot: gitProjectRoot,
    agentCwd: worktreePath,
    worktreePath,
  };
}

export async function removeWorktree({
  projectRoot,
  worktreePath,
  execFile = defaultExecFile,
}: {
  projectRoot: string;
  worktreePath: string;
  execFile?: ExecFileLike;
}): Promise<void> {
  try {
    await execFile("git", ["worktree", "remove", "--force", worktreePath], {
      cwd: projectRoot,
    });
  } catch {
    await rm(worktreePath, { recursive: true, force: true });
  }
}

async function resolveGitProjectRoot(
  cwd: string,
  execFile: ExecFileLike,
): Promise<string | null> {
  try {
    const result = await execFile("git", ["rev-parse", "--show-toplevel"], {
      cwd,
    });
    return await realpath(result.stdout.trim());
  } catch {
    return null;
  }
}
