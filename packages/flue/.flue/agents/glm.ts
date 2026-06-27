import { type AgentRouteHandler, defineAgent } from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { glmProfile } from "../shared/glm.js";
import { createAgentWorktree } from "../shared/worktrees.js";

export const description =
  "glm (GLM-5.2) is a senior full-stack engineer that works in its own per-delegation checkout.";

export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

export default defineAgent((ctx) => {
  const cwd = createAgentWorktree("glm", ctx.id);

  return {
    profile: glmProfile,
    sandbox: local({ cwd }),
  };
});
