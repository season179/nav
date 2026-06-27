import {
  defineAgent,
  defineWorkflow,
  type WorkflowRouteHandler,
} from "@flue/runtime";
import * as v from "valibot";
import {
  buildTitlePrompt,
  normalizeGeneratedTitle,
  TITLE_MODEL,
} from "../shared/session-title.js";

const titleResult = v.object({ title: v.string() });

export const route: WorkflowRouteHandler = async (_c, next) => {
  await next();
};

export default defineWorkflow({
  agent: defineAgent(() => ({
    instructions:
      "You write concise, specific chat titles. Return only the requested structured result.",
    model: TITLE_MODEL,
    thinkingLevel: "minimal",
    tools: [],
  })),
  input: v.object({
    transcript: v.array(
      v.object({
        role: v.picklist(["user", "assistant"]),
        text: v.string(),
      }),
    ),
  }),
  output: titleResult,
  async run({ harness, input }) {
    const session = await harness.session();
    const response = await session.prompt(buildTitlePrompt(input.transcript), {
      result: titleResult,
      thinkingLevel: "minimal",
    });
    const title = normalizeGeneratedTitle(response.data.title);

    if (!title) {
      throw new Error("Generated title was empty.");
    }

    return { title };
  },
});
