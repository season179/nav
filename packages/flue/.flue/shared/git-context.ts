import { execFileSync } from "node:child_process";
import { existsSync, realpathSync, statSync } from "node:fs";
import path from "node:path";

export type GitContext = {
  gitRoot: string;
  subpath: string;
};

type GitRootResolution =
  | { ok: true; gitRoot: string }
  | { ok: false; reason: string };

export type GitContextResolution =
  | { ok: true; context: GitContext }
  | { ok: false; reason: string };

const git = (cwd: string, args: string[]) =>
  execFileSync("git", ["-C", cwd, ...args], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  }).trim();

const canonicalDirectory = (input: string, label: string) => {
  if (!path.isAbsolute(input)) {
    throw new Error(`${label} must be an absolute path.`);
  }

  if (!existsSync(input)) {
    throw new Error(`${label} does not exist.`);
  }

  const real = path.normalize(realpathSync(input));

  if (!statSync(real).isDirectory()) {
    throw new Error(`${label} is not a directory.`);
  }

  return real;
};

const isContainedSubpath = (subpath: string) =>
  subpath === "" || (!subpath.startsWith("..") && !path.isAbsolute(subpath));

export const validateGitContext = (gitRoot: string, subpath: string) => {
  const root = canonicalDirectory(path.resolve(gitRoot), "git root");
  const actualGitRoot = canonicalDirectory(
    path.resolve(git(root, ["rev-parse", "--show-toplevel"])),
    "git root",
  );

  if (actualGitRoot !== root) {
    throw new Error("git root does not match its canonical repository root.");
  }

  if (path.isAbsolute(subpath)) {
    throw new Error("project subpath must be relative.");
  }

  const projectRoot = canonicalDirectory(
    path.resolve(root, subpath || "."),
    "project root",
  );
  const canonicalSubpath = path.relative(root, projectRoot);

  if (!isContainedSubpath(canonicalSubpath)) {
    throw new Error("project path must live inside its git root.");
  }

  return {
    gitRoot: root,
    subpath: canonicalSubpath,
  } satisfies GitContext;
};

export const resolveGitRoot = (projectRoot: string): GitRootResolution => {
  let projectPath: string;

  try {
    projectPath = canonicalDirectory(path.resolve(projectRoot), "project root");
  } catch {
    return { ok: false, reason: "project path is unavailable" };
  }

  try {
    const gitRoot = canonicalDirectory(
      path.resolve(git(projectPath, ["rev-parse", "--show-toplevel"])),
      "git root",
    );
    const context = validateGitContext(
      gitRoot,
      path.relative(gitRoot, projectPath),
    );

    return { ok: true, gitRoot: context.gitRoot };
  } catch {
    return { ok: false, reason: "project is not a git repo" };
  }
};

export const resolveGitContext = (
  projectRoot: string,
): GitContextResolution => {
  let projectPath: string;

  try {
    projectPath = canonicalDirectory(path.resolve(projectRoot), "project root");
  } catch {
    return { ok: false, reason: "project path is unavailable" };
  }

  const root = resolveGitRoot(projectPath);

  if (!root.ok) {
    return root;
  }

  try {
    git(root.gitRoot, ["rev-parse", "--verify", "HEAD"]);
  } catch {
    return { ok: false, reason: "project has no commits yet" };
  }

  try {
    return {
      ok: true,
      context: validateGitContext(
        root.gitRoot,
        path.relative(root.gitRoot, projectPath),
      ),
    };
  } catch {
    return { ok: false, reason: "project path escapes its git root" };
  }
};
