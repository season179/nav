import path from "node:path";
import type { AgentRouteHandler } from "@flue/runtime";
import { type GitContext, validateGitContext } from "./git-context.js";
import { createAgentWorktree } from "./worktrees.js";

const MAX_DELEGATION_CONTEXTS = 256;
const delegationContexts = new Map<string, GitContext>();

const rememberDelegationContext = (id: string, context: GitContext) => {
  if (delegationContexts.size >= MAX_DELEGATION_CONTEXTS) {
    const oldest = delegationContexts.keys().next().value;

    if (oldest) {
      delegationContexts.delete(oldest);
    }
  }

  delegationContexts.set(id, context);
};

export const requireDelegationCtx = (id: string) => {
  const context = delegationContexts.get(id);

  if (!context) {
    throw new Error(`Missing delegation context for ${id}.`);
  }

  return context;
};

export const createDelegateRoute = (): AgentRouteHandler => async (c, next) => {
  const id = c.req.param("id");
  const gitRoot = c.req.header("X-Nav-Repo-Root");
  const subpath = c.req.header("X-Nav-Subpath");

  if (!id) {
    throw new Error("Missing delegate session id.");
  }

  if (!gitRoot || subpath == null) {
    throw new Error("Missing delegate git context headers.");
  }

  const context = validateGitContext(gitRoot, subpath);

  rememberDelegationContext(id, context);

  try {
    await next();
  } finally {
    delegationContexts.delete(id);
  }
};

export const resolveDelegateCwd = (agent: string, id: string) => {
  const context = requireDelegationCtx(id);
  const worktree = createAgentWorktree(agent, id, context.gitRoot);

  return path.join(worktree, context.subpath);
};
