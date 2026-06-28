import {
  defineAgent,
  defineWorkflow,
  type WorkflowRouteHandler,
} from "@flue/runtime";
import {
  buildRequestClassifierPrompt,
  REQUEST_CLASSIFIER_MODEL,
  requestClassificationResult,
  requestClassifierInput,
} from "../shared/request-classifier.js";

export const route: WorkflowRouteHandler = async (_c, next) => {
  await next();
};

export default defineWorkflow({
  agent: defineAgent(() => ({
    instructions:
      "You classify coding-assistant requests. Return only the requested structured result.",
    model: REQUEST_CLASSIFIER_MODEL,
    thinkingLevel: "minimal",
    tools: [],
  })),
  input: requestClassifierInput,
  output: requestClassificationResult,
  async run({ harness, input }) {
    const session = await harness.session();
    const response = await session.prompt(buildRequestClassifierPrompt(input), {
      result: requestClassificationResult,
      thinkingLevel: "minimal",
    });

    return response.data;
  },
});
