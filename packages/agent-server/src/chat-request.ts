import {
  convertToModelMessages,
  type ModelMessage,
  safeValidateUIMessages,
  type UIMessage,
} from "ai";

const DIRECT_MESSAGE_ID = "direct-message";

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

interface RequestMessage {
  role?: unknown;
  content?: unknown;
  parts?: unknown;
}

export interface ChatMessages {
  modelMessages: ModelMessage[];
  uiMessages: UIMessage[];
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
    .map((part) => {
      if (!part || typeof part !== "object") {
        return "";
      }

      const maybeTextPart = part as { text?: unknown; type?: unknown };
      return maybeTextPart.type === "text" &&
        typeof maybeTextPart.text === "string"
        ? maybeTextPart.text
        : "";
    })
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

const createSingleUserMessage = (text: string): UIMessage => ({
  id: DIRECT_MESSAGE_ID,
  parts: [{ text, type: "text" }],
  role: "user",
});

const isUnsupportedAttachmentPart = (type: string): boolean =>
  type === "file" || type === "reasoning-file" || type.startsWith("data-");

const assertSupportedMessages = (messages: UIMessage[]) => {
  for (const message of messages) {
    if (message.role === "system") {
      throw new ChatRequestError(
        400,
        "System messages are not accepted from clients.",
      );
    }

    for (const part of message.parts) {
      const { type } = part;

      if (isUnsupportedAttachmentPart(type)) {
        throw new ChatRequestError(
          400,
          "File and data message parts are not supported yet.",
        );
      }

      if (message.role === "user" && type !== "text") {
        throw new ChatRequestError(
          400,
          "Only text user messages are supported yet.",
        );
      }
    }
  }
};

const hasUserTextMessage = (messages: UIMessage[]): boolean =>
  messages.some(
    (message) =>
      message.role === "user" &&
      message.parts.some(
        (part) => part.type === "text" && part.text.trim().length > 0,
      ),
  );

const toChatMessages = async (
  uiMessages: UIMessage[],
): Promise<ChatMessages> => {
  assertSupportedMessages(uiMessages);
  if (!hasUserTextMessage(uiMessages)) {
    throw new ChatRequestError(400, "Expected a non-empty user message.");
  }

  try {
    return {
      modelMessages: await convertToModelMessages(uiMessages, {
        ignoreIncompleteToolCalls: true,
      }),
      uiMessages,
    };
  } catch {
    throw new ChatRequestError(400, "Expected convertible UI messages.");
  }
};

export const readChatMessages = async (
  request: Request,
): Promise<ChatMessages> => {
  const body = await parseJsonBody(request);

  const directMessageText = getTextFromMessage(body.message).trim();
  if (directMessageText) {
    return toChatMessages([createSingleUserMessage(directMessageText)]);
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

  return toChatMessages(validated.data);
};
