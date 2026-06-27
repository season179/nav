import { registerProvider } from "@flue/runtime";

const PROVIDER_ID = "deepseek";

let registered = false;

/**
 * Register the DeepSeek catalog provider for both v4-pro and v4-flash.
 *
 * `deepseek` is a pi-ai catalog provider. Registering with only `apiKey`
 * preserves catalog metadata such as baseUrl, wire protocol, reasoning, and
 * thinking map.
 */
export function ensureDeepseekProvider(): void {
  if (registered) {
    return;
  }

  const apiKey = process.env.DEEPSEEK_API_KEY?.trim();

  if (!apiKey) {
    console.warn(
      "[nav] DEEPSEEK_API_KEY is not set; the deepseek-pro and deepseek-flash agents are unavailable until it is configured.",
    );
    return;
  }

  registerProvider(PROVIDER_ID, { apiKey });
  registered = true;
}
