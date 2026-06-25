import type { MessageProps } from "@/components/ai-elements/message";
import type { ToolPart } from "@/components/ai-elements/tool";
import type { ChatMessage, Message, ToolMessage } from "../types.ts";

export type AiElementsChatMessage = {
  kind: "message";
  id: string;
  role: ChatMessage["role"];
  from: MessageProps["from"];
  text: string;
  createdAt: string;
};

export type AiElementsToolMessage = {
  kind: "tool";
  id: string;
  toolCallId: string;
  toolName: string;
  state: ToolPart["state"];
  input: ToolPart["input"];
  output: string | undefined;
  errorText: ToolPart["errorText"];
};

export type AiElementsTranscriptItem =
  | AiElementsChatMessage
  | AiElementsToolMessage;

const messageRoleToFrom = {
  assistant: "assistant",
  error: "assistant",
  user: "user",
} satisfies Record<ChatMessage["role"], MessageProps["from"]>;

const toolStateToAiElementsState = {
  completed: "output-available",
  done: "output-available",
  failed: "output-error",
  running: "input-available",
} satisfies Record<string, ToolPart["state"]>;
type KnownToolState = keyof typeof toolStateToAiElementsState;

export function adaptMessagesForAiElements(
  messages: Message[],
): AiElementsTranscriptItem[] {
  return messages.map((message) => {
    if (message.role === "tool") {
      return adaptToolMessage(message);
    }
    return adaptChatMessage(message);
  });
}

function adaptChatMessage(message: ChatMessage): AiElementsChatMessage {
  return {
    kind: "message",
    id: message.id,
    role: message.role,
    from: messageRoleToFrom[message.role],
    text: message.text,
    createdAt: message.createdAt,
  };
}

function adaptToolMessage(message: ToolMessage): AiElementsToolMessage {
  const state = mapToolState(message.state);
  const failed = state === "output-error";

  return {
    kind: "tool",
    id: message.id,
    toolCallId: message.toolCallId,
    toolName: message.toolName,
    state,
    input: {},
    output: failed || !message.detail ? undefined : message.detail,
    errorText: failed ? message.detail || "Tool call failed" : undefined,
  };
}

function mapToolState(state: ToolMessage["state"]): ToolPart["state"] {
  return (
    toolStateToAiElementsState[state as KnownToolState] ?? "input-streaming"
  );
}
