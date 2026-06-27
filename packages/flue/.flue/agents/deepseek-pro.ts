import { type AgentRouteHandler, defineAgent } from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { deepseekProProfile } from "../shared/deepseek.js";
import {
  createDelegateRoute,
  resolveDelegateCwd,
} from "../shared/delegate-runtime.js";

export const description =
  "deepseek-pro (DeepSeek V4 Pro) is a junior full-stack engineer that works in its own per-delegation checkout.";

export const route: AgentRouteHandler = createDelegateRoute();

export default defineAgent((ctx) => {
  const cwd = resolveDelegateCwd("deepseek-pro", ctx.id);

  return {
    profile: deepseekProProfile,
    sandbox: local({ cwd }),
  };
});
