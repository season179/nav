import type { MessageProps } from "@/components/ai-elements/message";
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
  state: ToolMessage["state"];
  detail: string;
};

export type AiElementsTranscriptItem =
  | AiElementsChatMessage
  | AiElementsToolMessage;

const messageRoleToFrom = {
  assistant: "assistant",
  error: "assistant",
  user: "user",
} satisfies Record<ChatMessage["role"], MessageProps["from"]>;

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
  return {
    kind: "tool",
    id: message.id,
    toolCallId: message.toolCallId,
    toolName: message.toolName,
    // TODO: Map this to AI Elements tool states after Step 1.4 generates tool.tsx.
    state: message.state,
    detail: message.detail,
  };
}
