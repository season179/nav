import { type AgentRouteHandler, defineAgent } from "@flue/runtime";

export const description = "Says hello from Nav's first Flue workspace agent.";

export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

export default defineAgent(() => ({
  model: "openai/gpt-5.5",
  instructions:
    "You are Nav's first Flue agent. Keep responses friendly, concise, and useful while saying hello from the Flue workspace.",
}));
