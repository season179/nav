import { type AgentRouteHandler, defineAgent } from "@flue/runtime";
import { runCodexTaskTool } from "../shared/codex.js";

export const description =
  "Uses Codex local auth to delegate bounded coding-agent tasks.";

export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

export default defineAgent(() => ({
  model: "openai/gpt-5.5",
  instructions:
    "You help plan coding work. When a request needs Codex local execution, use run_codex_task with a narrow, explicit prompt. Keep responses concise and include the Codex thread ID when one is returned.",
  tools: [runCodexTaskTool],
}));
