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

export const readLatestUserPrompt = async (
  request: Request,
): Promise<string> => {
  const body = await parseJsonBody(request);

  const directMessageText = getTextFromMessage(body.message);
  if (directMessageText.trim()) {
    return directMessageText.trim();
  }

  if (!Array.isArray(body.messages)) {
    throw new ChatRequestError(400, "Expected a messages array.");
  }

  const latestUserMessage = [...body.messages]
    .reverse()
    .find(
      (message): message is RequestMessage =>
        !!message &&
        typeof message === "object" &&
        (message as RequestMessage).role === "user",
    );

  const prompt = getTextFromMessage(latestUserMessage).trim();
  if (!prompt) {
    throw new ChatRequestError(400, "Expected a non-empty user message.");
  }

  return prompt;
};
