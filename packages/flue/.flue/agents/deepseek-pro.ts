import { type AgentRouteHandler, defineAgent } from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { deepseekProProfile } from "../shared/deepseek.js";
import { createAgentWorktree } from "../shared/worktrees.js";

export const description =
  "deepseek-pro (DeepSeek V4 Pro) is a junior full-stack engineer that works in its own per-delegation checkout.";

export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

export default defineAgent((ctx) => {
  const cwd = createAgentWorktree("deepseek-pro", ctx.id);

  return {
    profile: deepseekProProfile,
    sandbox: local({ cwd }),
  };
});
