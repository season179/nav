import { useEffect, useRef, useState } from "react";

const tokenFormatter = new Intl.NumberFormat("en-US");
const sessionModeOptions = [
  { value: "local", label: "Work locally" },
  { value: "worktree", label: "New worktree" },
];

export default function Composer({
  connected,
  modelInfo,
  newSessionMode,
  running,
  stopPending,
  onNewSessionModeChange,
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
        <span className="composer-meta-left">
          <SessionModeMenu
            disabled={!connected || running}
            mode={newSessionMode}
            onModeChange={onNewSessionModeChange}
          />
          <span
            className="composer-model"
            id="composer-model"
            aria-live="polite"
          >
            {modelInfo?.label ?? ""}
          </span>
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

function SessionModeMenu({ disabled, mode, onModeChange }) {
  const [open, setOpen] = useState(false);
  const menuRef = useRef(null);

  useEffect(() => {
    if (!open) {
      return undefined;
    }

    function closeOnOutsidePointer(event) {
      if (!menuRef.current?.contains(event.target)) {
        setOpen(false);
      }
    }

    document.addEventListener("pointerdown", closeOnOutsidePointer);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
    };
  }, [open]);

  useEffect(() => {
    if (disabled) {
      setOpen(false);
    }
  }, [disabled]);

  function selectMode(value) {
    onModeChange(value);
    setOpen(false);
  }

  function closeOnEscape(event) {
    if (event.key === "Escape") {
      setOpen(false);
    }
  }

  return (
    <span className="composer-session-mode" ref={menuRef}>
      <button
        type="button"
        id="new-session-mode"
        className="session-mode-trigger"
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((isOpen) => !isOpen)}
        onKeyDown={closeOnEscape}
      >
        <span>{sessionModeLabel(mode)}</span>
        <span className="session-mode-chevron" aria-hidden="true">
          v
        </span>
      </button>
      {open ? (
        <div
          className="session-mode-menu"
          role="menu"
          onKeyDown={closeOnEscape}
        >
          <div className="session-mode-menu-title">Start in</div>
          {sessionModeOptions.map((option) => (
            <button
              key={option.value}
              type="button"
              className="session-mode-option"
              role="menuitemradio"
              aria-checked={option.value === mode ? "true" : "false"}
              onClick={() => selectMode(option.value)}
            >
              <span>{option.label}</span>
              <span className="session-mode-check" aria-hidden="true">
                {option.value === mode ? "✓" : ""}
              </span>
            </button>
          ))}
        </div>
      ) : null}
    </span>
  );
}

function sessionModeLabel(mode) {
  return (
    sessionModeOptions.find((option) => option.value === mode)?.label ??
    sessionModeOptions[0].label
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
