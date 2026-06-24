import { resolve } from "node:path";
import {
  type AgentRouteHandler,
  defineAgent,
  type ThinkingLevel,
} from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { backendServices } from "../services.js";

export const description =
  "Single-user coding agent backing the nav Electron app.";

export const route: AgentRouteHandler = async (_c, next) => next();

const defaultWorkspace =
  process.env.NAV_AGENT_CWD ?? resolve(process.cwd(), "..");

export default defineAgent(async (context) => {
  const session = await backendServices.catalog.get(context.id);
  const selection = session ?? backendServices.models.defaultSelection();

  return {
    model: backendServices.models.specifier(selection),
    thinkingLevel: selection.thinkingLevel as ThinkingLevel,
    instructions:
      "You are nav's local coding agent. Work inside the selected workspace, make focused code changes, and report concrete verification results.",
    cwd: session?.agentCwd ?? defaultWorkspace,
    sandbox: local(),
  };
});
