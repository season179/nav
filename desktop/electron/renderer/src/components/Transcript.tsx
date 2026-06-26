import { useMemo } from "react";
import {
  Conversation,
  ConversationContent,
  ConversationEmptyState,
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
    <Conversation
      className="min-h-0 bg-background"
      aria-label="Chat transcript"
    >
      <ConversationContent
        className="mx-auto w-full max-w-3xl gap-6 px-5 py-8 md:px-6"
        id="message-list"
      >
        {transcriptItems.length === 0 ? (
          <ConversationEmptyState
            className="min-h-[45vh]"
            title="Ready when you are"
            description="Start a thread and the conversation will appear here."
          />
        ) : (
          transcriptItems.map((item) =>
            item.kind === "tool" ? (
              <ToolMessageItem item={item} key={item.id} />
            ) : (
              <ChatMessageItem
                item={item}
                key={item.id}
                showTimestamp={timestampMessageIds.has(item.id)}
              />
            ),
          )
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
    <AiMessage className="max-w-[88%]" from={item.from}>
      <MessageContent>
        {item.role === "assistant" ? (
          <MessageResponse className="text-[0.95rem] leading-7">
            {item.text}
          </MessageResponse>
        ) : (
          <span className="whitespace-pre-wrap text-sm">{item.text}</span>
        )}
        {showTimestamp ? (
          <time
            className="text-muted-foreground text-[11px]"
            dateTime={item.createdAt}
          >
            {formatTimestamp(new Date(item.createdAt))}
          </time>
        ) : null}
      </MessageContent>
    </AiMessage>
  );
}

function ToolMessageItem({ item }: { item: AiElementsToolMessage }) {
  return (
    <AiMessage className="max-w-full" from="assistant">
      <MessageContent className="w-full">
        <Tool
          className="rounded-lg border bg-card shadow-sm"
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
                  <MessageResponse className="text-sm leading-6">
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
