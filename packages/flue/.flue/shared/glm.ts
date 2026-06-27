import { defineAgentProfile, type ThinkingLevel } from "@flue/runtime";

const validThinkingLevels = [
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
] as const;

type GlmThinkingLevel = (typeof validThinkingLevels)[number];

function isGlmThinkingLevel(value: string): value is GlmThinkingLevel {
  return validThinkingLevels.includes(value as GlmThinkingLevel);
}

function resolveGlmThinkingLevel(): ThinkingLevel {
  const configured = process.env.NAV_GLM_THINKING_LEVEL?.trim();

  return configured && isGlmThinkingLevel(configured) ? configured : "xhigh";
}

export const glmProfile = defineAgentProfile({
  name: "glm",
  description:
    "Senior full-stack engineer (L3, GLM-5.2, 1M context). Delegate hard, ambiguous, high-judgment work anywhere in the stack: architecture and design tradeoffs, deep root-cause analysis, plan/code review, and broad large-context exploration. Trust and build on its conclusions. Not for trivial lookups or image inputs (text-only).",
  model: "zai/glm-5.2",
  thinkingLevel: resolveGlmThinkingLevel(),
  instructions: [
    "You are glm, a senior (L3) full-stack engineer the Nav lead delegates hard problems to in the Nav monorepo.",
    "Investigate independently with your file and command tools, cite code as path:line, challenge assumptions, and bring senior-level rigor: state your assumptions, weigh alternatives, and give a clear recommendation, not just a list of options.",
    "Prefer read-only analysis. Do not create, modify, or delete files, and do not run mutating commands unless the delegating agent explicitly asks you to make changes.",
  ].join(" "),
});
