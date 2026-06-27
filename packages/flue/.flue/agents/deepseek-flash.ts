import { type AgentRouteHandler, defineAgent } from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { deepseekFlashProfile } from "../shared/deepseek.js";
import { createAgentWorktree } from "../shared/worktrees.js";

export const description =
  "deepseek-flash (DeepSeek V4 Flash) is a fast junior engineer that works in its own per-delegation checkout.";

export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

export default defineAgent((ctx) => {
  const cwd = createAgentWorktree("deepseek-flash", ctx.id);

  return {
    profile: deepseekFlashProfile,
    sandbox: local({ cwd }),
  };
});
