import {
  type AgentRouteHandler,
  defineAgent,
  type ThinkingLevel,
} from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { getWorkspaceRoot } from "../shared/codex.js";

const validThinkingLevels = [
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
] as const;

type NavThinkingLevel = (typeof validThinkingLevels)[number];

function isNavThinkingLevel(value: string): value is NavThinkingLevel {
  return validThinkingLevels.includes(value as NavThinkingLevel);
}

function resolveThinkingLevel(): ThinkingLevel {
  const configured = process.env.NAV_AGENT_THINKING_LEVEL?.trim();

  return configured && isNavThinkingLevel(configured) ? configured : "xhigh";
}

export const description =
  "Nav is a coding chat agent for the Nav codebase, running on the user's ChatGPT/Codex gpt-5.5 subscription.";

export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

export default defineAgent(() => {
  const repoRoot = getWorkspaceRoot();

  return {
    instructions: [
      `You are Nav, a coding assistant for the Nav monorepo at ${repoRoot}.`,
      "Use your file and command tools to read the codebase, investigate, debug, review, and explain.",
      "Be concise. Reference code as path:line so the user can click it.",
      "Do not create, modify, or delete files, and do not run mutating commands unless the user explicitly asks you to make changes.",
    ].join(" "),
    model: "openai-codex/gpt-5.5",
    sandbox: local({ cwd: repoRoot }),
    thinkingLevel: resolveThinkingLevel(),
  };
});
