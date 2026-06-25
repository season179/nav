import { useMemo } from "react";
import {
  Conversation,
  ConversationContent,
  ConversationScrollButton,
} from "@/components/ai-elements/conversation";
import {
  Message as AiMessage,
  MessageContent,
  MessageResponse,
} from "@/components/ai-elements/message";
import {
  Tool,
  ToolContent,
  ToolHeader,
  ToolInput,
  ToolOutput,
} from "@/components/ai-elements/tool";
import {
  type AiElementsChatMessage,
  type AiElementsToolMessage,
  adaptMessagesForAiElements,
} from "../lib/ai-elements-adapter.ts";
import type { Message } from "../types.ts";

export default function Transcript({ messages }: { messages: Message[] }) {
  const transcriptItems = useMemo(
    () => adaptMessagesForAiElements(messages),
    [messages],
  );
  const timestampMessageIds = useMemo(
    () => timestampVisibleMessageIds(messages),
    [messages],
  );

  return (
    <Conversation className="chat" aria-label="Chat transcript">
      <ConversationContent className="message-list" id="message-list">
        {transcriptItems.map((item) =>
          item.kind === "tool" ? (
            <ToolMessageItem item={item} key={item.id} />
          ) : (
            <ChatMessageItem
              item={item}
              key={item.id}
              showTimestamp={timestampMessageIds.has(item.id)}
            />
          ),
        )}
      </ConversationContent>
      {transcriptItems.length > 0 ? (
        <ConversationScrollButton aria-label="Scroll to latest message" />
      ) : null}
    </Conversation>
  );
}

function ChatMessageItem({
  item,
  showTimestamp,
}: {
  item: AiElementsChatMessage;
  showTimestamp: boolean;
}) {
  return (
    <AiMessage className={`message message-${item.role}`} from={item.from}>
      <MessageContent className="message-content">
        {item.role === "assistant" ? (
          <MessageResponse className="message-response">
            {item.text}
          </MessageResponse>
        ) : (
          <span className="message-text">{item.text}</span>
        )}
        {showTimestamp ? (
          <time className="message-time" dateTime={item.createdAt}>
            {formatTimestamp(new Date(item.createdAt))}
          </time>
        ) : null}
      </MessageContent>
    </AiMessage>
  );
}

function ToolMessageItem({ item }: { item: AiElementsToolMessage }) {
  return (
    <AiMessage className="message message-tool-wrapper" from="assistant">
      <MessageContent className="message-tool-content">
        <Tool
          className="message-tool"
          data-tool-call-id={item.toolCallId || undefined}
          defaultOpen={item.state !== "input-available"}
        >
          <ToolHeader
            state={item.state}
            toolName={item.toolName}
            type="dynamic-tool"
          />
          <ToolContent>
            <ToolInput input={item.input} />
            <ToolOutput
              errorText={item.errorText}
              output={
                item.output ? (
                  <MessageResponse className="message-response">
                    {item.output}
                  </MessageResponse>
                ) : undefined
              }
            />
          </ToolContent>
        </Tool>
      </MessageContent>
    </AiMessage>
  );
}

function timestampVisibleMessageIds(messages: Message[]): Set<string> {
  const visibleIds = new Set<string>();
  let lastPartyMessage: Message | null = null;
  let lastPartyRole: string | null = null;

  for (const message of messages) {
    if (message.role !== "user" && message.role !== "assistant") {
      continue;
    }
    if (lastPartyMessage && lastPartyRole === message.role) {
      visibleIds.delete(lastPartyMessage.id);
    }
    visibleIds.add(message.id);
    lastPartyMessage = message;
    lastPartyRole = message.role;
  }

  return visibleIds;
}

function formatTimestamp(date: Date): string {
  const day = String(date.getDate()).padStart(2, "0");
  const month = date.toLocaleString("en-US", { month: "short" });
  const hours = String(date.getHours()).padStart(2, "0");
  const minutes = String(date.getMinutes()).padStart(2, "0");
  return `${day} ${month} ${hours}:${minutes}`;
}
