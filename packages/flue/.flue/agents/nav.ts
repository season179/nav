import {
  type AgentRouteHandler,
  defineAgent,
  type ThinkingLevel,
} from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { makeConsult, makeConsultPanel } from "../shared/delegation.js";
import { resolveGitContext } from "../shared/git-context.js";
import {
  DEFAULT_NAV_MODEL_SPEC,
  resolveSessionProject,
} from "../shared/nav-projects.js";
import {
  getLatestOrchestratorContext,
  type OrchestratorTurnContext,
  prepareOrchestratorTurn,
} from "../shared/orchestrator.js";

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

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const readAgentPromptBody = async (
  request: Request,
): Promise<Record<string, unknown> | null> => {
  try {
    const body: unknown = await request.clone().json();

    return isObject(body) ? body : null;
  } catch {
    return null;
  }
};

export const route: AgentRouteHandler = async (c, next) => {
  if (c.req.raw.method === "POST") {
    const sessionId = c.req.param("id");
    const body = await readAgentPromptBody(c.req.raw);
    const message = typeof body?.message === "string" ? body.message : "";
    const hasImages = Array.isArray(body?.images) && body.images.length > 0;

    if (sessionId) {
      const project = resolveSessionProject(sessionId);
      const git = resolveGitContext(project.path);

      if (project.orchestratorEnabled && git.ok) {
        await prepareOrchestratorTurn({
          git: git.context,
          hasImages,
          message,
          project,
          sessionId,
          signal: c.req.raw.signal,
        });
      }
    }
  }

  await next();
};

const buildInstructions = (
  cwd: string,
  autoApproveEdits: boolean,
  fleetUnavailableReason: string | null,
  orchestratorEnabled: boolean,
  orchestratorContext: OrchestratorTurnContext | null,
) =>
  [
    `You are Nav, a coding assistant working in the project at ${cwd}.`,
    "Use your file and command tools to read the codebase, investigate, debug, review, and explain.",
    "Be concise. Reference code as path:line so the user can click it.",
    autoApproveEdits
      ? "This project allows edits by default: when the user's goal requires changes, you may create, modify, delete files, and run mutating commands without asking for a separate approval."
      : "Do not create, modify, or delete files, and do not run mutating commands unless the user explicitly asks you to make changes.",
    fleetUnavailableReason
      ? `Work as a solo agent in this project; fleet unavailable: ${fleetUnavailableReason}.`
      : orchestratorEnabled
        ? buildOrchestratorInstructions(orchestratorContext)
        : [
            "You are the lead, coordinating a team of engineers who each work in their own separate checkout of this repo. Use consult to delegate one task to one engineer, or consult_panel to give the same task to several at once and compare.",
            "Route by difficulty, not domain: hard, ambiguous, or high-judgment work goes to glm; well-scoped mechanical work goes to deepseek-pro; small trivial fully-specified tasks go to deepseek-flash.",
            "Each result includes a worktree path. Read its real changes with git -C <worktree> diff, take the best parts of each, and write the final result in the active project checkout. Never delegate image-based tasks because all delegates are text-only.",
          ].join(" "),
  ].join(" ");

const delegateSummary = (context: OrchestratorTurnContext) => {
  if (context.delegateResults.length === 0) {
    return "No delegate result rows are available for this turn.";
  }

  return context.delegateResults
    .map((result) =>
      [
        `${result.agent} status: ${result.status}.`,
        result.worktree ? `Worktree: ${result.worktree}.` : "No worktree.",
        result.error ? `Error: ${result.error}.` : null,
        result.answer ? `Answer:\n${result.answer}` : null,
      ]
        .filter(Boolean)
        .join(" "),
    )
    .join("\n\n");
};

const buildOrchestratorInstructions = (
  context: OrchestratorTurnContext | null,
) => {
  const base =
    "Orchestrator mode is enabled for this project. Server-side orchestration owns delegation; do not run another delegate panel for this turn. Work in the active project checkout only when producing the final result.";

  if (!context) {
    return `${base} No orchestrator turn context was prepared, so handle this request directly.`;
  }

  const activeThread = context.active
    ? "This session is inside an active orchestrated thread."
    : "This session has not entered an active orchestrated thread.";
  const difficulty = context.difficulty ?? "unknown";

  if (context.mode === "direct") {
    return [
      base,
      activeThread,
      `The current request difficulty is ${difficulty}; no delegate panel was run.`,
      context.error ? `Routing note: ${context.error}` : null,
      "Handle this turn directly, while preserving any relevant prior orchestrated-thread context from the conversation.",
    ]
      .filter(Boolean)
      .join(" ");
  }

  if (context.status === "failed") {
    return [
      base,
      activeThread,
      `The current request difficulty is ${difficulty}; the delegate panel failed.`,
      context.error ? `Panel error: ${context.error}` : null,
      "Continue directly in the active checkout.",
    ]
      .filter(Boolean)
      .join(" ");
  }

  if (context.status === "pending") {
    return [
      base,
      activeThread,
      `The current request difficulty is ${difficulty}; the delegate panel has not produced final stored results yet.`,
      "Handle this turn directly unless usable delegate output is available in the stored context.",
      delegateSummary(context),
    ].join(" ");
  }

  return [
    base,
    activeThread,
    `The current request difficulty is ${difficulty}; the server already sent the same task to glm and deepseek-pro.`,
    context.status === "partial"
      ? "The panel partially completed; use any successful delegate output and account for failed delegates."
      : "Both delegates completed.",
    "Inspect each successful delegate worktree with git diff, judge both solutions, take the useful parts from each, and write the final synthesized solution in the active project checkout.",
    delegateSummary(context),
  ].join(" ");
};

export default defineAgent((ctx) => {
  const project = resolveSessionProject(ctx.id);
  const git = resolveGitContext(project.path);
  const tools =
    git.ok && !project.orchestratorEnabled
      ? [makeConsult(git.context), makeConsultPanel(git.context)]
      : [];
  const orchestratorContext = project.orchestratorEnabled
    ? getLatestOrchestratorContext(ctx.id)
    : null;

  return {
    instructions: buildInstructions(
      project.path,
      project.autoApproveEdits,
      git.ok ? null : git.reason,
      project.orchestratorEnabled,
      orchestratorContext,
    ),
    model: project.modelSpec ?? DEFAULT_NAV_MODEL_SPEC,
    sandbox: local({ cwd: project.path }),
    tools,
    thinkingLevel: resolveThinkingLevel(),
  };
});
