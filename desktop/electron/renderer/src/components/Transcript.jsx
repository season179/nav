import { useEffect, useMemo, useRef } from "react";
import { renderMarkdown } from "../lib/markdown.js";

export default function Transcript({ messages }) {
  const listRef = useRef(null);
  const timestampMessageIds = useMemo(
    () => timestampVisibleMessageIds(messages),
    [messages],
  );

  useEffect(() => {
    listRef.current?.lastElementChild?.scrollIntoView({ block: "end" });
  });

  return (
    <section className="chat" aria-label="Chat transcript">
      <ol ref={listRef} className="message-list" id="message-list">
        {messages.map((message) => (
          <Message
            key={message.id}
            message={message}
            showTimestamp={timestampMessageIds.has(message.id)}
          />
        ))}
      </ol>
    </section>
  );
}

function Message({ message, showTimestamp }) {
  if (message.role === "tool") {
    return <ToolMessage message={message} />;
  }

  return (
    <li className={`message message-${message.role}`}>
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
    </li>
  );
}

function MarkdownText({ text }) {
  const html = useMemo(() => renderMarkdown(text), [text]);
  return (
    <div
      className="message-text markdown"
      /* biome-ignore lint/security/noDangerouslySetInnerHtml: Assistant markdown is sanitized with DOMPurify first. */
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

function ToolMessage({ message }) {
  return (
    <li
      className={`message message-tool message-tool-${message.state}`}
      data-tool-call-id={message.toolCallId || undefined}
    >
      <span className="message-role">{toolMarker(message.state)}</span>
      <span className="tool-name">{message.toolName}</span>
      {message.detail ? (
        <span className="tool-detail">{previewText(message.detail)}</span>
      ) : null}
    </li>
  );
}

function timestampVisibleMessageIds(messages) {
  const visibleIds = new Set();
  let lastPartyMessage = null;
  let lastPartyRole = null;

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

function formatTimestamp(date) {
  const day = String(date.getDate()).padStart(2, "0");
  const month = date.toLocaleString("en-US", { month: "short" });
  const hours = String(date.getHours()).padStart(2, "0");
  const minutes = String(date.getMinutes()).padStart(2, "0");
  return `${day} ${month} ${hours}:${minutes}`;
}

function toolMarker(state) {
  switch (state) {
    case "running":
      return ">";
    case "failed":
      return "x";
    default:
      return "*";
  }
}

function previewText(text) {
  const firstLine = text.split("\n", 1)[0];
  return firstLine.length > 120 ? `${firstLine.slice(0, 117)}...` : firstLine;
}
