import * as v from "valibot";

const MAX_PRIOR_ASSISTANT_CHARS = 2000;

export const REQUEST_CLASSIFIER_MODEL = "deepseek/deepseek-v4-flash";
export const requestDifficulty = v.picklist(["low", "medium", "high"]);
export const requestClassifierInput = v.object({
  priorAssistant: v.optional(v.string()),
  text: v.string(),
});
export const requestClassificationResult = v.object({
  difficulty: requestDifficulty,
  isPlanning: v.boolean(),
});

export type RequestDifficulty = "low" | "medium" | "high";
export type RequestClassification = {
  difficulty: RequestDifficulty;
  isPlanning: boolean;
};

const normalizeText = (value: string | undefined) =>
  (value ?? "").replace(/\s+/g, " ").trim();

const clipPriorAssistant = (value: string | undefined) => {
  const normalized = normalizeText(value);

  return normalized.length > MAX_PRIOR_ASSISTANT_CHARS
    ? normalized.slice(-MAX_PRIOR_ASSISTANT_CHARS)
    : normalized;
};

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const isRequestDifficulty = (value: unknown): value is RequestDifficulty =>
  value === "low" || value === "medium" || value === "high";

export const normalizeRequestClassification = (
  value: unknown,
): RequestClassification | null => {
  if (!isObject(value) || typeof value.isPlanning !== "boolean") {
    return null;
  }

  if (!isRequestDifficulty(value.difficulty)) {
    return null;
  }

  return {
    difficulty: value.difficulty,
    isPlanning: value.isPlanning,
  };
};

export const buildRequestClassifierPrompt = (input: {
  priorAssistant?: string;
  text: string;
}) => {
  const priorAssistant = clipPriorAssistant(input.priorAssistant);
  const requestText = normalizeText(input.text);

  return [
    "You classify a single request to a coding assistant.",
    "`isPlanning` is true when the request is mainly about planning, decomposing, designing an approach, or making a to-do list. It is false when the user is asking to directly execute, answer, explain, inspect, fix, or implement.",
    "`difficulty` is low for trivial or quick requests, medium for several-step work, and high for complex, large, risky, or ambiguous work.",
    "Use the prior assistant turn only to resolve short follow-ups like 'ok do it' or 'same for tests'.",
    "Return only the structured result.",
    priorAssistant
      ? `Prior assistant turn:\n${priorAssistant}`
      : "Prior assistant turn: none",
    `User request:\n${requestText}`,
  ].join("\n\n");
};
