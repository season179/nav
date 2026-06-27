import {
  convertToModelMessages,
  type ModelMessage,
  safeValidateUIMessages,
  type UIMessage,
} from "ai";

export class ChatRequestError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

interface ChatRequestBody {
  message?: unknown;
  messages?: unknown;
}

interface MessagePart {
  type?: unknown;
  text?: unknown;
}

interface RequestMessage {
  role?: unknown;
  content?: unknown;
  parts?: unknown;
}

interface TextMessage {
  role: "assistant" | "user";
  text: string;
}

const parseJsonBody = async (request: Request): Promise<ChatRequestBody> => {
  try {
    return (await request.json()) as ChatRequestBody;
  } catch {
    throw new ChatRequestError(400, "Expected a JSON request body.");
  }
};

const getTextFromParts = (parts: unknown): string => {
  if (!Array.isArray(parts)) {
    return "";
  }

  return parts
    .map((part: MessagePart) =>
      part?.type === "text" && typeof part.text === "string" ? part.text : "",
    )
    .join("");
};

const getTextFromMessage = (message: unknown): string => {
  if (!message || typeof message !== "object") {
    return "";
  }

  const requestMessage = message as RequestMessage;

  if (typeof requestMessage.content === "string") {
    return requestMessage.content;
  }

  return getTextFromParts(requestMessage.parts);
};

const toTextMessage = (message: unknown): TextMessage | undefined => {
  if (!message || typeof message !== "object") {
    return undefined;
  }

  const requestMessage = message as RequestMessage;
  const { role } = requestMessage;
  if (role !== "assistant" && role !== "user") {
    return undefined;
  }

  const text = getTextFromMessage(requestMessage).trim();
  return text ? { role, text } : undefined;
};

const formatPrompt = (messages: TextMessage[]): string => {
  const latestUserIndex = messages.findLastIndex(
    (message) => message.role === "user",
  );
  if (latestUserIndex === -1) {
    throw new ChatRequestError(400, "Expected a non-empty user message.");
  }

  const latestUserMessage = messages[latestUserIndex];
  const history = messages.slice(0, latestUserIndex);
  if (history.length === 0) {
    return latestUserMessage.text;
  }

  return [
    "Conversation so far:",
    ...history.map((message) => `${message.role}: ${message.text}`),
    "",
    "Current user request:",
    latestUserMessage.text,
  ].join("\n");
};

const toSingleUserModelMessage = async (
  text: string,
): Promise<ModelMessage[]> =>
  convertToModelMessages([
    {
      parts: [{ text, type: "text" }],
      role: "user",
    },
  ] satisfies Array<Omit<UIMessage, "id">>);

export const readChatMessages = async (
  request: Request,
): Promise<ModelMessage[]> => {
  const body = await parseJsonBody(request);

  const directMessageText = getTextFromMessage(body.message).trim();
  if (directMessageText) {
    return toSingleUserModelMessage(directMessageText);
  }

  if (!Array.isArray(body.messages)) {
    throw new ChatRequestError(400, "Expected a messages array.");
  }

  const validated = await safeValidateUIMessages<UIMessage>({
    messages: body.messages,
  });
  if (!validated.success) {
    throw new ChatRequestError(400, "Expected valid UI messages.");
  }

  const prompt = formatPrompt(
    validated.data
      .map(toTextMessage)
      .filter((message): message is TextMessage => message !== undefined),
  );
  if (!prompt) {
    throw new ChatRequestError(400, "Expected a non-empty user message.");
  }

  return toSingleUserModelMessage(prompt);
};
