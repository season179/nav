import { execSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { HttpProviderRegistration } from "@flue/runtime";
import { THINKING_LEVELS, type ThinkingLevel } from "./model-types.js";

export type ProviderRegistrationSpec = {
  provider: string;
  registration: HttpProviderRegistration;
};

export type SettingsModelDefinition = {
  provider: string;
  model: string;
  label: string;
  thinkingLevels: ThinkingLevel[];
  defaultThinkingLevel: ThinkingLevel;
  contextWindow: number;
  maxTokens?: number;
};

type NavSettings = {
  defaultModel?: {
    provider?: unknown;
    model?: unknown;
  };
  providers?: Record<string, unknown>;
};

type ProviderSettings = {
  name?: unknown;
  api?: unknown;
  baseUrl?: unknown;
  apiKey?: unknown;
  contextWindow?: unknown;
  maxTokens?: unknown;
  models?: unknown;
};

type ModelSettings = {
  id?: unknown;
  name?: unknown;
  reasoning?: unknown;
  thinkingLevelMap?: unknown;
  contextWindow?: unknown;
  maxTokens?: unknown;
};

type ModelSettingsSource = {
  defaultModel: { provider: string; model: string } | null;
  definitions: SettingsModelDefinition[];
  registrations: ProviderRegistrationSpec[];
};

type CodexAuthFile = {
  tokens?: {
    access_token?: unknown;
  };
};

export function loadModelSettings({
  env = process.env,
  settings,
  settingsPath,
}: {
  env?: NodeJS.ProcessEnv;
  settings?: unknown;
  settingsPath?: string;
} = {}): ModelSettingsSource {
  const parsed =
    settings === undefined
      ? readSettingsFile(settingsPathFor(env, settingsPath))
      : settings;
  return settingsSourceFromParsed(parsed, env);
}

export function normalizeProviderId(provider: string): string {
  return provider === "codex" ? "openai-codex" : provider;
}

export function normalizeApiId(api: string | null): string | null {
  return api === "codex-responses" ? "openai-codex-responses" : api;
}

function settingsSourceFromParsed(
  parsed: unknown,
  env: NodeJS.ProcessEnv,
): ModelSettingsSource {
  if (!parsed || typeof parsed !== "object") {
    return { defaultModel: null, definitions: [], registrations: [] };
  }

  const settings = parsed as NavSettings;
  const providers =
    settings.providers && typeof settings.providers === "object"
      ? settings.providers
      : {};
  const definitions: SettingsModelDefinition[] = [];
  const registrations = new Map<string, ProviderRegistrationSpec>();

  for (const [configuredProvider, rawProvider] of Object.entries(providers)) {
    if (!rawProvider || typeof rawProvider !== "object") {
      continue;
    }
    const providerSettings = rawProvider as ProviderSettings;
    const provider = normalizeProviderId(configuredProvider);
    const api = normalizeApiId(optionalString(providerSettings.api));
    const providerModels = Array.isArray(providerSettings.models)
      ? (providerSettings.models as ModelSettings[])
      : [];

    for (const modelSettings of providerModels) {
      const model = optionalString(modelSettings.id);
      if (!model) {
        continue;
      }
      const thinkingLevels = thinkingLevelsFor(modelSettings);
      definitions.push({
        provider,
        model,
        label: optionalString(modelSettings.name) ?? `${provider}/${model}`,
        thinkingLevels,
        defaultThinkingLevel: defaultThinkingLevel(thinkingLevels),
        contextWindow:
          optionalNumber(modelSettings.contextWindow) ??
          optionalNumber(providerSettings.contextWindow) ??
          0,
        maxTokens:
          optionalNumber(modelSettings.maxTokens) ??
          optionalNumber(providerSettings.maxTokens) ??
          undefined,
      });
    }

    const registration = providerRegistrationFor({
      api,
      env,
      provider,
      providerSettings,
      models: providerModels,
    });
    if (registration) {
      registrations.set(provider, { provider, registration });
    }
  }

  return {
    defaultModel: defaultModelFrom(settings.defaultModel),
    definitions,
    registrations: [...registrations.values()],
  };
}

function providerRegistrationFor({
  api,
  env,
  provider,
  providerSettings,
  models,
}: {
  api: string | null;
  env: NodeJS.ProcessEnv;
  provider: string;
  providerSettings: ProviderSettings;
  models: ModelSettings[];
}): HttpProviderRegistration | null {
  const baseUrl = optionalString(providerSettings.baseUrl);
  const apiKey =
    resolveSecret(providerSettings.apiKey, env) ??
    codexAccessTokenFor({ api, env, provider });
  const contextWindow = optionalNumber(providerSettings.contextWindow);
  const maxTokens = optionalNumber(providerSettings.maxTokens);
  const modelOverrides = modelRegistrationOverrides(models);

  if (!api && !baseUrl && !apiKey && !contextWindow && !maxTokens) {
    return Object.keys(modelOverrides).length > 0
      ? { models: modelOverrides }
      : null;
  }

  return {
    ...(api ? { api } : {}),
    ...(baseUrl ? { baseUrl } : {}),
    ...(apiKey ? { apiKey } : {}),
    ...(contextWindow ? { contextWindow } : {}),
    ...(maxTokens ? { maxTokens } : {}),
    ...(Object.keys(modelOverrides).length > 0
      ? { models: modelOverrides }
      : {}),
  };
}

function modelRegistrationOverrides(
  models: ModelSettings[],
): NonNullable<HttpProviderRegistration["models"]> {
  const overrides: NonNullable<HttpProviderRegistration["models"]> = {};
  for (const model of models) {
    const id = optionalString(model.id);
    if (!id) {
      continue;
    }
    const contextWindow = optionalNumber(model.contextWindow);
    const maxTokens = optionalNumber(model.maxTokens);
    if (contextWindow || maxTokens) {
      overrides[id] = {
        ...(contextWindow ? { contextWindow } : {}),
        ...(maxTokens ? { maxTokens } : {}),
      };
    }
  }
  return overrides;
}

function defaultModelFrom(
  value: NavSettings["defaultModel"],
): { provider: string; model: string } | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const provider = optionalString(value.provider);
  const model = optionalString(value.model);
  return provider && model
    ? { provider: normalizeProviderId(provider), model }
    : null;
}

function thinkingLevelsFor(model: ModelSettings): ThinkingLevel[] {
  if (model.reasoning === false) {
    return ["off"];
  }
  if (model.thinkingLevelMap && typeof model.thinkingLevelMap === "object") {
    const allowed = THINKING_LEVELS.filter((level) =>
      Object.hasOwn(model.thinkingLevelMap as object, level),
    );
    return allowed.length > 0 ? allowed : [...THINKING_LEVELS];
  }
  return [...THINKING_LEVELS];
}

function defaultThinkingLevel(levels: ThinkingLevel[]): ThinkingLevel {
  return levels.includes("medium") ? "medium" : (levels[0] ?? "off");
}

function readSettingsFile(settingsPath: string): unknown {
  if (!existsSync(settingsPath)) {
    return null;
  }
  try {
    return JSON.parse(readFileSync(settingsPath, "utf8")) as unknown;
  } catch {
    return null;
  }
}

function settingsPathFor(
  env: NodeJS.ProcessEnv,
  settingsPath?: string,
): string {
  return (
    settingsPath ??
    env.NAV_SETTINGS_PATH ??
    join(homedir(), ".nav", "settings.json")
  );
}

function codexAccessTokenFor({
  api,
  env,
  provider,
}: {
  api: string | null;
  env: NodeJS.ProcessEnv;
  provider: string;
}): string | null {
  if (provider !== "openai-codex" && api !== "openai-codex-responses") {
    return null;
  }

  return (
    optionalString(env.NAV_CODEX_ACCESS_TOKEN) ??
    optionalString(env.OPENAI_CODEX_ACCESS_TOKEN) ??
    optionalString(env.CODEX_ACCESS_TOKEN) ??
    codexAccessTokenFromFile(codexAuthPathFor(env))
  );
}

function codexAccessTokenFromFile(authPath: string): string | null {
  if (!existsSync(authPath)) {
    return null;
  }
  try {
    const auth = JSON.parse(readFileSync(authPath, "utf8")) as CodexAuthFile;
    return optionalString(auth.tokens?.access_token);
  } catch {
    return null;
  }
}

function codexAuthPathFor(env: NodeJS.ProcessEnv): string {
  return (
    env.NAV_CODEX_AUTH_PATH ??
    join(env.CODEX_HOME ?? join(homedir(), ".codex"), "auth.json")
  );
}

function resolveSecret(value: unknown, env: NodeJS.ProcessEnv): string | null {
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (!trimmed) {
      return null;
    }
    if (trimmed.startsWith("!")) {
      return commandSecret(trimmed.slice(1));
    }
    if (trimmed.startsWith("${") && trimmed.endsWith("}")) {
      return env[trimmed.slice(2, -1)] ?? null;
    }
    if (trimmed.startsWith("$")) {
      return env[trimmed.slice(1)] ?? null;
    }
    return env[trimmed] ?? trimmed;
  }

  if (!value || typeof value !== "object") {
    return null;
  }
  const secret = value as { envVar?: unknown; inline?: unknown };
  const envVar = optionalString(secret.envVar);
  if (envVar) {
    return env[envVar] ?? null;
  }
  return optionalString(secret.inline);
}

function commandSecret(command: string): string | null {
  if (!command.trim()) {
    return null;
  }
  try {
    return execSync(command, {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return null;
  }
}

function optionalString(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0
    ? value.trim()
    : null;
}

function optionalNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value > 0
    ? value
    : null;
}
