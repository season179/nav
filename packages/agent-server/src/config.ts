import { exec } from "node:child_process";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

import type { Api, Model, Models } from "@earendil-works/pi-ai";
import { InMemoryCredentialStore } from "@earendil-works/pi-ai";
import { builtinModels } from "@earendil-works/pi-ai/providers/all";

const execAsync = promisify(exec);

const PI_AGENT_DIR = join(homedir(), ".pi", "agent");
const PI_SETTINGS_PATH = join(PI_AGENT_DIR, "settings.json");
const PI_MODELS_PATH = join(PI_AGENT_DIR, "models.json");

export const SERVICE_NAME = "@nav/agent-server";
export const DEFAULT_AGENT_HOST = "127.0.0.1";
export const DEFAULT_AGENT_PORT = 3583;
export const NAV_WORKSPACE_CWD = "/Users/season/Personal/nav";

interface PiSettings {
  defaultProvider?: unknown;
  defaultModel?: unknown;
}

interface PiModelsConfig {
  providers?: Record<
    string,
    {
      apiKey?: unknown;
      headers?: unknown;
    }
  >;
}

export interface ResolvedPiModel {
  model: Model<Api>;
  models: Models;
  provider: string;
  modelId: string;
}

const readJsonFile = async <T>(path: string): Promise<T | undefined> => {
  try {
    return JSON.parse(await readFile(path, "utf8")) as T;
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") {
      return undefined;
    }
    throw error;
  }
};

const asNonEmptyString = (value: unknown): string | undefined =>
  typeof value === "string" && value.trim() ? value.trim() : undefined;

const resolveEnvReference = (value: string): string | undefined => {
  const braced = value.match(/^\$\{([A-Za-z_][A-Za-z0-9_]*)\}$/);
  const plain = value.match(/^\$([A-Za-z_][A-Za-z0-9_]*)$/);
  const envName = braced?.[1] ?? plain?.[1];

  return envName ? process.env[envName] : undefined;
};

const resolveSecret = async (
  rawValue: unknown,
): Promise<string | undefined> => {
  const value = asNonEmptyString(rawValue);
  if (!value) {
    return undefined;
  }

  const envValue = resolveEnvReference(value);
  if (envValue !== undefined) {
    return envValue;
  }

  if (value.startsWith("!")) {
    const command = value.slice(1).trim();
    if (!command) {
      return undefined;
    }

    const { stdout } = await execAsync(command, {
      shell: process.env.SHELL ?? "/bin/zsh",
      timeout: 5000,
    });

    return stdout.trim() || undefined;
  }

  return value;
};

const seedPiCredentials = async (
  credentials: InMemoryCredentialStore,
  provider: string,
) => {
  const modelsConfig = await readJsonFile<PiModelsConfig>(PI_MODELS_PATH);
  const providerConfig = modelsConfig?.providers?.[provider];
  const apiKey = await resolveSecret(providerConfig?.apiKey);

  if (!apiKey) {
    return;
  }

  await credentials.modify(provider, async () => ({
    key: apiKey,
    type: "api_key",
  }));
};

export const resolvePiModel = async (): Promise<ResolvedPiModel> => {
  const settings = await readJsonFile<PiSettings>(PI_SETTINGS_PATH);
  const provider =
    asNonEmptyString(process.env.NAV_AGENT_PROVIDER) ??
    asNonEmptyString(process.env.PI_PROVIDER) ??
    asNonEmptyString(settings?.defaultProvider);
  const modelId =
    asNonEmptyString(process.env.NAV_AGENT_MODEL) ??
    asNonEmptyString(process.env.PI_MODEL) ??
    asNonEmptyString(settings?.defaultModel);

  if (!provider || !modelId) {
    throw new Error(
      "No Pi model is configured. Set NAV_AGENT_PROVIDER/NAV_AGENT_MODEL or configure ~/.pi/agent/settings.json.",
    );
  }

  const credentials = new InMemoryCredentialStore();
  await seedPiCredentials(credentials, provider);

  const models = builtinModels({ credentials });
  const model = models.getModel(provider, modelId);

  if (!model) {
    throw new Error(`Pi model is not available: ${provider}/${modelId}.`);
  }

  return {
    model,
    modelId,
    models,
    provider,
  };
};
