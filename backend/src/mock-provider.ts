import { registerApiProvider, registerProvider } from "@flue/runtime";

export const NAV_MOCK_PROVIDER = "nav-mock";
export const NAV_MOCK_MODEL = "nav-smoke";

const NAV_MOCK_API = "nav-mock-api";
const MOCK_BASE_URL = "http://127.0.0.1/nav-mock";

type MockTextContent = {
  type: "text";
  text: string;
};

type MockAssistantMessage = {
  role: "assistant";
  content: MockTextContent[];
  api: string;
  provider: string;
  model: string;
  usage: {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
    totalTokens: number;
    cost: {
      input: number;
      output: number;
      cacheRead: number;
      cacheWrite: number;
      total: number;
    };
  };
  stopReason: "stop" | "aborted";
  errorMessage?: string;
  timestamp: number;
};

type MockAssistantEvent =
  | { type: "start"; partial: MockAssistantMessage }
  | { type: "text_start"; contentIndex: number; partial: MockAssistantMessage }
  | {
      type: "text_delta";
      contentIndex: number;
      delta: string;
      partial: MockAssistantMessage;
    }
  | {
      type: "text_end";
      contentIndex: number;
      content: string;
      partial: MockAssistantMessage;
    }
  | { type: "done"; reason: "stop"; message: MockAssistantMessage }
  | { type: "error"; reason: "aborted"; error: MockAssistantMessage };

type MockContext = {
  messages?: {
    role?: string;
    content?: unknown;
  }[];
};

type MockModel = {
  id?: string;
};

type MockStreamOptions = {
  signal?: AbortSignal;
  onResponse?: (
    response: { status: number; headers: Record<string, string> },
    model: MockModel,
  ) => unknown | Promise<unknown>;
};

class MockAssistantStream implements AsyncIterable<MockAssistantEvent> {
  readonly #events: MockAssistantEvent[];
  readonly #result: MockAssistantMessage;

  constructor(events: MockAssistantEvent[], result: MockAssistantMessage) {
    this.#events = events;
    this.#result = result;
  }

  async *[Symbol.asyncIterator](): AsyncIterator<MockAssistantEvent> {
    for (const event of this.#events) {
      await Promise.resolve();
      yield event;
    }
  }

  result(): Promise<MockAssistantMessage> {
    return Promise.resolve(this.#result);
  }
}

export function registerNavMockProvider(
  env: NodeJS.ProcessEnv = process.env,
): void {
  if (!isEnabled(env.NAV_MOCK_MODEL)) {
    return;
  }

  registerApiProvider({
    api: NAV_MOCK_API,
    stream: mockStream,
    streamSimple: mockStream,
  } as unknown as Parameters<typeof registerApiProvider>[0]);

  registerProvider(NAV_MOCK_PROVIDER, {
    api: NAV_MOCK_API,
    baseUrl: MOCK_BASE_URL,
    contextWindow: 128_000,
    maxTokens: 4_096,
    models: {
      [NAV_MOCK_MODEL]: {
        contextWindow: 128_000,
        maxTokens: 4_096,
      },
    },
    telemetry: {
      providerName: "nav.mock",
    },
  });
}

export function isNavMockModelEnabled(
  env: NodeJS.ProcessEnv = process.env,
): boolean {
  return isEnabled(env.NAV_MOCK_MODEL);
}

function mockStream(
  model: MockModel,
  context: MockContext,
  options?: MockStreamOptions,
): MockAssistantStream {
  const modelId = model.id ?? NAV_MOCK_MODEL;
  const prompt = lastUserText(context);
  const text = `nav mock response: accepted "${prompt || "empty prompt"}"`;
  const message = assistantMessage(text, modelId, "stop", prompt);

  if (options?.signal?.aborted) {
    const aborted = assistantMessage("", modelId, "aborted", prompt);
    return new MockAssistantStream(
      [{ type: "error", reason: "aborted", error: aborted }],
      aborted,
    );
  }

  const partial: MockAssistantMessage = { ...message, content: [] };
  const events: MockAssistantEvent[] = [
    { type: "start", partial },
    { type: "text_start", contentIndex: 0, partial },
    {
      type: "text_delta",
      contentIndex: 0,
      delta: text,
      partial: { ...message },
    },
    {
      type: "text_end",
      contentIndex: 0,
      content: text,
      partial: { ...message },
    },
    { type: "done", reason: "stop", message },
  ];

  void options?.onResponse?.({ status: 200, headers: {} }, model);

  return new MockAssistantStream(events, message);
}

function assistantMessage(
  text: string,
  model: string,
  stopReason: MockAssistantMessage["stopReason"],
  prompt: string,
): MockAssistantMessage {
  const input = estimateTokens(prompt);
  const output = estimateTokens(text);

  return {
    role: "assistant",
    content: text ? [{ type: "text", text }] : [],
    api: NAV_MOCK_API,
    provider: NAV_MOCK_PROVIDER,
    model,
    usage: {
      input,
      output,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: input + output,
      cost: {
        input: 0,
        output: 0,
        cacheRead: 0,
        cacheWrite: 0,
        total: 0,
      },
    },
    stopReason,
    errorMessage: stopReason === "aborted" ? "Request was aborted" : undefined,
    timestamp: Date.now(),
  };
}

function lastUserText(context: MockContext): string {
  const user = [...(context.messages ?? [])]
    .reverse()
    .find((message) => message.role === "user");

  return contentToText(user?.content).trim();
}

function contentToText(content: unknown): string {
  if (typeof content === "string") {
    return content;
  }
  if (!Array.isArray(content)) {
    return "";
  }

  return content
    .map((part) => {
      if (
        part &&
        typeof part === "object" &&
        "type" in part &&
        part.type === "text" &&
        "text" in part &&
        typeof part.text === "string"
      ) {
        return part.text;
      }
      return "";
    })
    .filter(Boolean)
    .join("\n");
}

function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}

function isEnabled(value: string | undefined): boolean {
  return value === "1" || value === "true" || value === "yes";
}
