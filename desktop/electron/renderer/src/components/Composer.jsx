import { useEffect, useRef, useState } from "react";

const tokenFormatter = new Intl.NumberFormat("en-US");

export default function Composer({
  connected,
  modelInfo,
  running,
  stopPending,
  onSend,
  onStop,
}) {
  const [text, setText] = useState("");
  const inputRef = useRef(null);

  useEffect(() => {
    const input = inputRef.current;
    if (!input) {
      return;
    }
    input.style.height = "auto";
    input.style.height = `${input.scrollHeight}px`;
  });

  useEffect(() => {
    if (connected) {
      inputRef.current?.focus();
    }
  }, [connected]);

  async function handleSubmit(event) {
    event.preventDefault();
    const message = text.trim();
    if (!message || !connected) {
      return;
    }

    setText("");
    await onSend(message);
  }

  function handleKeyDown(event) {
    if (event.nativeEvent.isComposing) {
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      event.currentTarget.form.requestSubmit();
    }
  }

  return (
    <form className="composer" id="composer" onSubmit={handleSubmit}>
      <div className="composer-row">
        <textarea
          ref={inputRef}
          id="composer-input"
          className="composer-input"
          aria-label="Message"
          placeholder="Send a message..."
          autoComplete="off"
          rows="1"
          disabled={!connected}
          value={text}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={handleKeyDown}
        />
        <button
          type="submit"
          id="composer-send"
          className="composer-send"
          disabled={!connected}
        >
          Send
        </button>
        {running ? (
          <button
            type="button"
            id="composer-stop"
            className="composer-stop"
            disabled={!connected || stopPending}
            onClick={onStop}
          >
            Stop
          </button>
        ) : null}
      </div>
      <div className="composer-meta">
        <span className="composer-model" id="composer-model" aria-live="polite">
          {modelInfo?.label ?? ""}
        </span>
        <span className="composer-meta-right">
          <span
            className="composer-thinking"
            id="composer-thinking"
            aria-live="polite"
          >
            {modelInfo?.thinking ?? ""}
          </span>
          <span className="composer-token-usage" id="composer-token-usage">
            {formatTokenUsage(modelInfo?.tokenUsage)}
          </span>
        </span>
      </div>
    </form>
  );
}

function formatTokenUsage(tokenUsage) {
  if (!tokenUsage?.contextWindow) {
    return "";
  }
  const used = Number.isFinite(tokenUsage.used) ? tokenUsage.used : 0;
  return `${tokenFormatter.format(used)}/${tokenFormatter.format(
    tokenUsage.contextWindow,
  )}`;
}
