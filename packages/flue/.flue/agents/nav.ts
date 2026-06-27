import {
  type AgentRouteHandler,
  defineAgent,
  type ThinkingLevel,
} from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { consult, consultPanel } from "../shared/delegation.js";
import { resolveSessionProject } from "../shared/nav-projects.js";

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
  "Nav is a coding chat agent for local projects, running on the user's ChatGPT/Codex gpt-5.5 subscription.";

export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

const buildInstructions = (cwd: string, fleet: boolean) =>
  [
    `You are Nav, a coding assistant working in the project at ${cwd}.`,
    "Use your file and command tools to read the codebase, investigate, debug, review, and explain.",
    "Be concise. Reference code as path:line so the user can click it.",
    "Do not create, modify, or delete files, and do not run mutating commands unless the user explicitly asks you to make changes.",
    fleet
      ? [
          "You are the lead, coordinating a team of engineers who each work in their own separate checkout of this repo. Use consult to delegate one task to one engineer, or consult_panel to give the same task to several at once and compare.",
          "Route by difficulty, not domain: hard, ambiguous, or high-judgment work goes to glm; well-scoped mechanical work goes to deepseek-pro; small trivial fully-specified tasks go to deepseek-flash.",
          "Each result includes a worktree path. Read its real changes with git -C <worktree> diff, take the best parts of each, and write the final result in your own checkout. Never delegate image-based tasks because all delegates are text-only.",
        ].join(" ")
      : "Work as a solo agent in this project. The delegate fleet is unavailable outside the default Nav project.",
  ].join(" ");

export default defineAgent((ctx) => {
  const project = resolveSessionProject(ctx.id);
  const tools = project.isDefault ? [consult, consultPanel] : [];

  return {
    instructions: buildInstructions(project.path, project.isDefault),
    model: "openai-codex/gpt-5.5",
    sandbox: local({ cwd: project.path }),
    tools,
    thinkingLevel: resolveThinkingLevel(),
  };
});
