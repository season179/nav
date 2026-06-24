import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useMemo, useRef } from "react";
import { renderMarkdown } from "../lib/markdown.ts";
import type { Message, ToolMessage } from "../types.ts";

export default function Transcript({ messages }: { messages: Message[] }) {
  const scrollRef = useRef<HTMLElement>(null);
  const rowVirtualizer = useVirtualizer({
    count: messages.length,
    estimateSize: () => 88,
    getItemKey: (index) => messages[index]?.id ?? index,
    getScrollElement: () => scrollRef.current,
    overscan: 6,
    useFlushSync: false,
  });
  const timestampMessageIds = useMemo(
    () => timestampVisibleMessageIds(messages),
    [messages],
  );

  // Only follow this transcript's own new messages. Without the dependency the
  // effect runs on every App re-render — including a background session's
  // events — and would yank the viewport down while the user reads history.
  useEffect(() => {
    if (messages.length === 0) {
      return;
    }
    rowVirtualizer.scrollToIndex(messages.length - 1, { align: "end" });
  }, [messages, rowVirtualizer]);

  return (
    <section ref={scrollRef} className="chat" aria-label="Chat transcript">
      <ol
        className="message-list"
        id="message-list"
        style={{ height: `${rowVirtualizer.getTotalSize()}px` }}
      >
        {rowVirtualizer.getVirtualItems().map((virtualRow) => {
          const message = messages[virtualRow.index];
          if (!message) {
            return null;
          }

          return (
            <li
              key={virtualRow.key}
              ref={rowVirtualizer.measureElement}
              className="message-virtual-row"
              data-index={virtualRow.index}
              style={{ transform: `translateY(${virtualRow.start}px)` }}
            >
              <MessageRow
                message={message}
                showTimestamp={timestampMessageIds.has(message.id)}
              />
            </li>
          );
        })}
      </ol>
    </section>
  );
}

function MessageRow({
  message,
  showTimestamp,
}: {
  message: Message;
  showTimestamp: boolean;
}) {
  if (message.role === "tool") {
    return <ToolMessageRow message={message} />;
  }

  return (
    <div className={`message message-${message.role}`}>
      {message.role === "assistant" ? (
        <MarkdownText text={message.text} />
      ) : (
        <span className="message-text">{message.text}</span>
      )}
      {showTimestamp ? (
        <time className="message-time" dateTime={message.createdAt}>
          {formatTimestamp(new Date(message.createdAt))}
        </time>
      ) : null}
    </div>
  );
}

function MarkdownText({ text }: { text: string }) {
  const html = useMemo(() => renderMarkdown(text), [text]);
  return (
    <div
      className="message-text markdown"
      /* Assistant markdown is sanitized with DOMPurify first. */
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

function ToolMessageRow({ message }: { message: ToolMessage }) {
  return (
    <div
      className={`message message-tool message-tool-${message.state}`}
      data-tool-call-id={message.toolCallId || undefined}
    >
      <span className="message-role">{toolMarker(message.state)}</span>
      <span className="tool-name">{message.toolName}</span>
      {message.detail ? (
        <span className="tool-detail">{previewText(message.detail)}</span>
      ) : null}
    </div>
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

function toolMarker(state: string): string {
  switch (state) {
    case "running":
      return ">";
    case "failed":
      return "x";
    default:
      return "*";
  }
}

function previewText(text: string): string {
  const firstLine = text.split("\n", 1)[0];
  return firstLine.length > 120 ? `${firstLine.slice(0, 117)}...` : firstLine;
}
