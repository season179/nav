export const THINKING_LEVELS = [
  "off",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
] as const;

export type ThinkingLevel = (typeof THINKING_LEVELS)[number];

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

const DEFAULT_MODEL_SPECIFIER = "anthropic/claude-sonnet-4-6";

const DEFAULT_MODELS: ModelDefinition[] = [
  {
    provider: "anthropic",
    model: "claude-sonnet-4-6",
    label: "Claude Sonnet 4.6",
    thinkingLevels: [...THINKING_LEVELS],
    defaultThinkingLevel: "medium",
    contextWindow: 200_000,
  },
  {
    provider: "openai",
    model: "gpt-5",
    label: "GPT-5",
    thinkingLevels: [...THINKING_LEVELS],
    defaultThinkingLevel: "medium",
    contextWindow: 400_000,
  },
];

export class ModelCatalog {
  readonly #definitions: ModelDefinition[];
  readonly #env: NodeJS.ProcessEnv;

  constructor({
    definitions = DEFAULT_MODELS,
    env = process.env,
  }: {
    definitions?: ModelDefinition[];
    env?: NodeJS.ProcessEnv;
  } = {}) {
    this.#definitions = definitions;
    this.#env = env;
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
      this.#env.NAV_DEFAULT_MODEL ?? DEFAULT_MODEL_SPECIFIER,
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
    const definition = this.find(provider, model);

    return {
      provider,
      model,
      thinkingLevel: this.coerceThinkingLevel(thinkingLevel, definition),
    };
  }

  switchThinking(
    selection: Pick<ModelSelection, "provider" | "model">,
    thinkingLevel: string | null | undefined,
  ): ModelSelection {
    const definition = this.find(selection.provider, selection.model);

    return {
      provider: selection.provider,
      model: selection.model,
      thinkingLevel: this.coerceThinkingLevel(thinkingLevel, definition),
    };
  }

  modelInfo(selection: ModelSelection): ModelInfo {
    const definition = this.find(selection.provider, selection.model);
    const thinkingLevel = this.coerceThinkingLevel(
      selection.thinkingLevel,
      definition,
    );

    return {
      label: definition.label,
      provider: selection.provider,
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
    return `${selection.provider}/${selection.model}`;
  }

  find(provider: string, model: string): ModelDefinition {
    return (
      this.#definitions.find(
        (definition) =>
          definition.provider === provider && definition.model === model,
      ) ?? {
        provider,
        model,
        label: `${provider}/${model}`,
        thinkingLevels: [...THINKING_LEVELS],
        defaultThinkingLevel: "medium",
        contextWindow: 0,
      }
    );
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

function parseModelSpecifier(specifier: string): {
  provider: string;
  model: string;
} {
  const [provider, ...modelParts] = specifier.split("/");
  const model = modelParts.join("/");

  if (!provider || !model) {
    return parseModelSpecifier(DEFAULT_MODEL_SPECIFIER);
  }

  return { provider, model };
}

function isThinkingLevel(
  value: string | null | undefined,
): value is ThinkingLevel {
  return THINKING_LEVELS.includes(value as ThinkingLevel);
}
