import { type AgentRouteHandler, defineAgent } from "@flue/runtime";
import { runCodexTaskTool } from "../shared/codex.js";

export const description =
  "Nav is a coding agent that delegates bounded local coding tasks through Codex.";

export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

export default defineAgent(() => ({
  model: "openai/gpt-5.5",
  instructions:
    "You are Nav, a coding agent for the Nav codebase. Help with implementation, debugging, review, and code explanation. When a request needs Codex local execution, use run_codex_task with a narrow, explicit prompt that includes the task goal and verification criteria. Keep responses concise and include the Codex thread ID when one is returned.",
  tools: [runCodexTaskTool],
}));
