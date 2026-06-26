import { registerProvider } from "@flue/runtime";
import {
  isNavMockModelEnabled,
  NAV_MOCK_MODEL,
  NAV_MOCK_PROVIDER,
} from "./mock-provider.js";
import {
  loadModelSettings,
  normalizeProviderId,
  type ProviderRegistrationSpec,
  type SettingsModelDefinition,
} from "./model-settings.js";
import { THINKING_LEVELS, type ThinkingLevel } from "./model-types.js";

export type ModelOption = {
  provider: string;
  model: string;
  label: string;
  thinkingLevels: ThinkingLevel[];
};

export type TokenUsage = {
  used: number;
  contextWindow: number;
};

export type ModelInfo = {
  label: string;
  provider: string | null;
  model: string | null;
  thinking: ThinkingLevel | null;
  thinkingLevels: ThinkingLevel[];
  tokenUsage: TokenUsage | null;
};

export type ModelSelection = {
  provider: string;
  model: string;
  thinkingLevel: ThinkingLevel;
};

type ModelDefinition = ModelOption & {
  defaultThinkingLevel: ThinkingLevel;
  contextWindow: number;
};

const DEFAULT_MODEL_SPECIFIER = "openai/gpt-5";
const MOCK_MODEL_SPECIFIER = `${NAV_MOCK_PROVIDER}/${NAV_MOCK_MODEL}`;

const DEFAULT_MODELS: ModelDefinition[] = [
  {
    provider: "openai",
    model: "gpt-5",
    label: "GPT-5",
    thinkingLevels: [...THINKING_LEVELS],
    defaultThinkingLevel: "medium",
    contextWindow: 400_000,
  },
];

const MOCK_MODEL: ModelDefinition = {
  provider: NAV_MOCK_PROVIDER,
  model: NAV_MOCK_MODEL,
  label: "nav Offline Smoke Mock",
  thinkingLevels: ["off"],
  defaultThinkingLevel: "off",
  contextWindow: 128_000,
};

export class ModelCatalog {
  readonly #definitions: ModelDefinition[];
  readonly #env: NodeJS.ProcessEnv;
  readonly #settingsDefault: ModelSelection | null;
  readonly #providerRegistrations: ProviderRegistrationSpec[];

  constructor({
    definitions = DEFAULT_MODELS,
    env = process.env,
    settings,
    settingsPath,
  }: {
    definitions?: ModelDefinition[];
    env?: NodeJS.ProcessEnv;
    settings?: unknown;
    settingsPath?: string;
  } = {}) {
    const configured = loadModelSettings({ env, settings, settingsPath });
    const configuredDefinitions = configured.definitions.map(toModelDefinition);
    const baseDefinitions =
      configuredDefinitions.length > 0 ? configuredDefinitions : definitions;

    this.#definitions = isNavMockModelEnabled(env)
      ? [...baseDefinitions, MOCK_MODEL]
      : baseDefinitions;
    this.#env = env;
    this.#settingsDefault = configured.defaultModel
      ? {
          ...configured.defaultModel,
          thinkingLevel: this.find(
            configured.defaultModel.provider,
            configured.defaultModel.model,
          ).defaultThinkingLevel,
        }
      : null;
    this.#providerRegistrations = configured.registrations;
  }

  list(): ModelOption[] {
    return this.#definitions.map(
      ({ provider, model, label, thinkingLevels }) => ({
        provider,
        model,
        label,
        thinkingLevels,
      }),
    );
  }

  defaultSelection(): ModelSelection {
    const parsed = parseModelSpecifier(
      isNavMockModelEnabled(this.#env)
        ? MOCK_MODEL_SPECIFIER
        : (this.#env.NAV_DEFAULT_MODEL ??
            (this.#settingsDefault
              ? this.specifier(this.#settingsDefault)
              : DEFAULT_MODEL_SPECIFIER)),
    );
    const definition = this.find(parsed.provider, parsed.model);

    return {
      provider: parsed.provider,
      model: parsed.model,
      thinkingLevel: this.coerceThinkingLevel(
        this.#env.NAV_DEFAULT_THINKING_LEVEL,
        definition,
      ),
    };
  }

  defaultModelInfo(): ModelInfo {
    return this.modelInfo(this.defaultSelection());
  }

  resolveSelection({
    provider,
    model,
    thinkingLevel,
  }: {
    provider: string;
    model: string;
    thinkingLevel?: string | null;
  }): ModelSelection {
    const normalizedProvider = normalizeProviderId(provider);
    const definition = this.find(normalizedProvider, model);

    return {
      provider: normalizedProvider,
      model,
      thinkingLevel: this.coerceThinkingLevel(thinkingLevel, definition),
    };
  }

  switchThinking(
    selection: Pick<ModelSelection, "provider" | "model">,
    thinkingLevel: string | null | undefined,
  ): ModelSelection {
    const provider = normalizeProviderId(selection.provider);
    const definition = this.find(provider, selection.model);

    return {
      provider,
      model: selection.model,
      thinkingLevel: this.coerceThinkingLevel(thinkingLevel, definition),
    };
  }

  modelInfo(selection: ModelSelection): ModelInfo {
    const provider = normalizeProviderId(selection.provider);
    const definition = this.find(provider, selection.model);
    const thinkingLevel = this.coerceThinkingLevel(
      selection.thinkingLevel,
      definition,
    );

    return {
      label: definition.label,
      provider,
      model: selection.model,
      thinking: thinkingLevel,
      thinkingLevels: definition.thinkingLevels,
      tokenUsage: {
        used: 0,
        contextWindow: definition.contextWindow,
      },
    };
  }

  specifier(selection: Pick<ModelSelection, "provider" | "model">): string {
    return `${normalizeProviderId(selection.provider)}/${selection.model}`;
  }

  find(provider: string, model: string): ModelDefinition {
    const normalizedProvider = normalizeProviderId(provider);
    return (
      this.#definitions.find(
        (definition) =>
          definition.provider === normalizedProvider &&
          definition.model === model,
      ) ?? {
        provider: normalizedProvider,
        model,
        label: `${normalizedProvider}/${model}`,
        thinkingLevels: [...THINKING_LEVELS],
        defaultThinkingLevel: "medium",
        contextWindow: 0,
      }
    );
  }

  providerRegistrations(): ProviderRegistrationSpec[] {
    return this.#providerRegistrations;
  }

  registerProviders(): void {
    for (const { provider, registration } of this.#providerRegistrations) {
      registerProvider(provider, registration);
    }
  }

  private coerceThinkingLevel(
    value: string | null | undefined,
    definition: ModelDefinition,
  ): ThinkingLevel {
    return isThinkingLevel(value) && definition.thinkingLevels.includes(value)
      ? value
      : definition.defaultThinkingLevel;
  }
}

function toModelDefinition(
  definition: SettingsModelDefinition,
): ModelDefinition {
  return {
    provider: definition.provider,
    model: definition.model,
    label: definition.label,
    thinkingLevels: definition.thinkingLevels,
    defaultThinkingLevel: definition.defaultThinkingLevel,
    contextWindow: definition.contextWindow,
  };
}

function parseModelSpecifier(specifier: string): {
  provider: string;
  model: string;
} {
  const [provider, ...modelParts] = specifier.split("/");
  const model = modelParts.join("/");

  if (!provider || !model) {
    return parseModelSpecifier(DEFAULT_MODEL_SPECIFIER);
  }

  return { provider: normalizeProviderId(provider), model };
}

function isThinkingLevel(
  value: string | null | undefined,
): value is ThinkingLevel {
  return THINKING_LEVELS.includes(value as ThinkingLevel);
}
