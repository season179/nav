import { type AgentRouteHandler, defineAgent } from "@flue/runtime";
import { local } from "@flue/runtime/node";
import {
  createDelegateRoute,
  resolveDelegateCwd,
} from "../shared/delegate-runtime.js";
import { glmProfile } from "../shared/glm.js";

export const description =
  "glm (GLM-5.2) is a senior full-stack engineer that works in its own per-delegation checkout.";

export const route: AgentRouteHandler = createDelegateRoute();

export default defineAgent((ctx) => {
  const cwd = resolveDelegateCwd("glm", ctx.id);

  return {
    profile: glmProfile,
    sandbox: local({ cwd }),
  };
});
