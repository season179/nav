import { resolve } from "node:path";
import { type AgentRouteHandler, defineAgent } from "@flue/runtime";
import { local } from "@flue/runtime/node";

export const description =
  "Single-user coding agent backing the nav Electron app.";

export const route: AgentRouteHandler = async (_c, next) => next();

const defaultWorkspace =
  process.env.NAV_AGENT_CWD ?? resolve(process.cwd(), "..");

export default defineAgent(() => ({
  model: process.env.NAV_DEFAULT_MODEL ?? "anthropic/claude-sonnet-4-6",
  instructions:
    "You are nav's local coding agent. Work inside the selected workspace, make focused code changes, and report concrete verification results.",
  cwd: defaultWorkspace,
  sandbox: local(),
}));
