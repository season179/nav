import { registerProvider } from "@flue/runtime";

const PROVIDER_ID = "zai";

let registered = false;

/**
 * Register the Z.ai GLM Coding Plan provider so `zai/glm-5.2` resolves.
 *
 * `zai` is a pi-ai catalog provider. Registering with only `apiKey` preserves
 * catalog metadata such as baseUrl, wire protocol, reasoning, and thinking map.
 */
export function ensureZaiProvider(): void {
  if (registered) {
    return;
  }

  const apiKey = process.env.ZAI_API_KEY?.trim();

  if (!apiKey) {
    console.warn(
      "[nav] ZAI_API_KEY is not set; the glm agent is unavailable until it is configured.",
    );
    return;
  }

  registerProvider(PROVIDER_ID, { apiKey });
  registered = true;
}
