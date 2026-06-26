import {
  defineAgent,
  defineWorkflow,
  type WorkflowRouteHandler,
  type WorkflowRunsHandler,
} from "@flue/runtime";
import { codexTaskInputSchema, runCodexTask } from "../shared/codex.js";

const agent = defineAgent(() => ({
  model: false,
  instructions:
    "Run finite Codex tasks through the application-owned Codex SDK wrapper.",
}));

export const route: WorkflowRouteHandler = async (_c, next) => {
  await next();
};

export const runs: WorkflowRunsHandler = async (_c, next) => {
  await next();
};

export default defineWorkflow({
  agent,
  input: codexTaskInputSchema,
  async run({ input }) {
    return await runCodexTask(input);
  },
});
