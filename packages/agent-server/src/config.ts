import { exec } from "node:child_process";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

import type { PiHarnessSettings } from "@ai-sdk/harness-pi";

const execAsync = promisify(exec);

const PI_AGENT_DIR = join(homedir(), ".pi", "agent");
const PI_SETTINGS_PATH = join(PI_AGENT_DIR, "settings.json");
const PI_MODELS_PATH = join(PI_AGENT_DIR, "models.json");

export const SERVICE_NAME = "@nav/agent-server";
export const DEFAULT_AGENT_HOST = "127.0.0.1";
export const DEFAULT_AGENT_PORT = 3583;
export const NAV_WORKSPACE_CWD = "/Users/season/Personal/nav";

type PiThinkingLevel = NonNullable<PiHarnessSettings["thinkingLevel"]>;

const THINKING_LEVELS = new Set<PiThinkingLevel>([
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
]);

interface PiSettings {
  defaultProvider?: unknown;
  defaultModel?: unknown;
  defaultThinkingLevel?: unknown;
}

interface PiProviderConfig {
  apiKey?: unknown;
  baseUrl?: unknown;
  headers?: unknown;
}

interface PiModelsConfig {
  providers?: Record<string, PiProviderConfig>;
}

export interface ResolvedPiHarnessSettings extends PiHarnessSettings {
  provider?: string;
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

const toEnvPrefix = (provider: string): string =>
  provider
    .replace(/[^A-Za-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .toUpperCase();

export const toPiModelReference = (
  provider: string | undefined,
  model: string,
): string => {
  if (!provider || model.startsWith(`${provider}/`)) {
    return model;
  }

  return `${provider}/${model}`;
};

const resolveThinkingLevel = (value: unknown): PiThinkingLevel | undefined => {
  const thinkingLevel = asNonEmptyString(value);
  if (!thinkingLevel) {
    return undefined;
  }

  if (!THINKING_LEVELS.has(thinkingLevel as PiThinkingLevel)) {
    throw new Error(`Unsupported Pi thinking level: ${thinkingLevel}.`);
  }

  return thinkingLevel as PiThinkingLevel;
};

const resolveProviderEnv = async (
  provider: string | undefined,
): Promise<Record<string, string> | undefined> => {
  if (!provider) {
    return undefined;
  }

  const modelsConfig = await readJsonFile<PiModelsConfig>(PI_MODELS_PATH);
  const providerConfig = modelsConfig?.providers?.[provider];
  const apiKey = await resolveSecret(providerConfig?.apiKey);
  const baseUrl = await resolveSecret(providerConfig?.baseUrl);

  const customEnv: Record<string, string> = {};
  const prefix = toEnvPrefix(provider);

  if (apiKey) {
    customEnv[`${prefix}_API_KEY`] = apiKey;
  }

  if (baseUrl) {
    customEnv[`${prefix}_BASE_URL`] = baseUrl;
  }

  return Object.keys(customEnv).length > 0 ? customEnv : undefined;
};

export const resolvePiHarnessSettings =
  async (): Promise<ResolvedPiHarnessSettings> => {
    const settings = await readJsonFile<PiSettings>(PI_SETTINGS_PATH);
    const provider =
      asNonEmptyString(process.env.NAV_AGENT_PROVIDER) ??
      asNonEmptyString(process.env.PI_PROVIDER) ??
      asNonEmptyString(settings?.defaultProvider);
    const model =
      asNonEmptyString(process.env.NAV_AGENT_MODEL) ??
      asNonEmptyString(process.env.PI_MODEL) ??
      asNonEmptyString(settings?.defaultModel);
    const thinkingLevel = resolveThinkingLevel(
      asNonEmptyString(process.env.NAV_AGENT_THINKING_LEVEL) ??
        asNonEmptyString(process.env.PI_THINKING_LEVEL) ??
        settings?.defaultThinkingLevel,
    );
    const customEnv = await resolveProviderEnv(provider);

    if (!model) {
      throw new Error(
        "No Pi model is configured. Set NAV_AGENT_MODEL/PI_MODEL or configure ~/.pi/agent/settings.json.",
      );
    }

    return {
      ...(customEnv ? { auth: { customEnv } } : {}),
      ...(provider ? { provider } : {}),
      model: toPiModelReference(provider, model),
      ...(thinkingLevel ? { thinkingLevel } : {}),
    };
  };
