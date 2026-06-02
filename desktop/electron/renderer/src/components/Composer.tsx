import { useCallback, useEffect, useRef, useState } from "react";
import type {
  ModelInfo,
  ModelOption,
  SessionMode,
  TokenUsage,
} from "../types.ts";

const tokenFormatter = new Intl.NumberFormat("en-US");
const sessionModeOptions: { value: SessionMode; label: string }[] = [
  { value: "local", label: "Local" },
  { value: "worktree", label: "Worktree" },
];
const thinkingLevelDetails: Record<string, string> = {
  off: "No reasoning",
  minimal: "Very brief reasoning",
  low: "Light reasoning",
  medium: "Balanced reasoning",
  high: "Deeper reasoning",
  xhigh: "Maximum reasoning",
};

export default function Composer({
  connected,
  modelInfo,
  modelOptions,
  modelSwitching,
  newSessionMode,
  running,
  stopPending,
  onModelChange,
  onNewSessionModeChange,
  onSend,
  onStop,
  onThinkingChange,
}: {
  connected: boolean;
  modelInfo: ModelInfo | null;
  modelOptions: ModelOption[];
  modelSwitching: boolean;
  newSessionMode: SessionMode;
  running: boolean;
  stopPending: boolean;
  onModelChange: (option: ModelOption) => void;
  onNewSessionModeChange: (mode: SessionMode) => void;
  onSend: (message: string) => void | Promise<void>;
  onStop: () => void;
  onThinkingChange: (level: string) => void;
}) {
  const [text, setText] = useState("");
  const inputRef = useRef<HTMLTextAreaElement>(null);

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

  async function handleSubmit(event: React.SyntheticEvent<HTMLFormElement>) {
    event.preventDefault();
    const message = text.trim();
    if (!message || !connected) {
      return;
    }

    setText("");
    await onSend(message);
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) {
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      event.currentTarget.form?.requestSubmit();
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
          placeholder="Tell nav what to do"
          autoComplete="off"
          rows={1}
          disabled={!connected}
          value={text}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={handleKeyDown}
        />
        <button
          type="submit"
          id="composer-send"
          className="composer-send"
          aria-label="Send message"
          title="Send message"
          disabled={!connected}
        >
          ↑
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
            disabled={!connected}
            mode={newSessionMode}
            onModeChange={onNewSessionModeChange}
          />
          <ModelMenu
            disabled={!connected || modelSwitching}
            modelInfo={modelInfo}
            options={modelOptions}
            onModelChange={onModelChange}
          />
        </span>
        <span className="composer-meta-right">
          <ThinkingMenu
            disabled={!connected || modelSwitching}
            modelInfo={modelInfo}
            onThinkingChange={onThinkingChange}
          />
          <span className="composer-token-usage" id="composer-token-usage">
            {formatTokenUsage(modelInfo?.tokenUsage)}
          </span>
        </span>
      </div>
    </form>
  );
}

function ThinkingMenu({
  disabled,
  modelInfo,
  onThinkingChange,
}: {
  disabled: boolean;
  modelInfo: ModelInfo | null;
  onThinkingChange: (level: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [focusedIndex, setFocusedIndex] = useState(0);
  const menuRef = useRef<HTMLSpanElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const rawLevels = modelInfo?.thinkingLevels;
  const levels = Array.isArray(rawLevels) ? rawLevels : [];
  const current = modelInfo?.thinking ?? levels[0] ?? "";
  const hasChoices = levels.length > 1;

  const closeMenu = useCallback(() => {
    setOpen(false);
    window.setTimeout(() => {
      triggerRef.current?.focus();
    }, 0);
  }, []);

  const openMenu = useCallback(() => {
    setFocusedIndex(0);
    setOpen(true);
  }, []);

  useEffect(() => {
    if (!open) {
      return undefined;
    }

    function closeOnOutsidePointer(event: PointerEvent) {
      if (!menuRef.current?.contains(event.target as Node)) {
        closeMenu();
      }
    }

    document.addEventListener("pointerdown", closeOnOutsidePointer);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
    };
  }, [open, closeMenu]);

  useEffect(() => {
    if (disabled || !hasChoices) {
      closeMenu();
    }
  }, [disabled, hasChoices, closeMenu]);

  useEffect(() => {
    itemRefs.current = itemRefs.current.slice(0, levels.length);
  }, [levels.length]);

  useEffect(() => {
    if (!open) {
      return;
    }
    window.setTimeout(() => {
      itemRefs.current[focusedIndex]?.focus();
    }, 0);
  }, [focusedIndex, open]);

  function selectThinking(level: string) {
    onThinkingChange(level);
    closeMenu();
  }

  function handleTriggerKeyDown(event: React.KeyboardEvent<HTMLButtonElement>) {
    if (event.key === "Escape") {
      closeMenu();
    }
  }

  function handleMenuKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      closeMenu();
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusThinkingOption(focusedIndex + 1);
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      focusThinkingOption(focusedIndex - 1);
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const focusedLevel = levels[focusedIndex];
      if (focusedLevel) {
        selectThinking(focusedLevel);
      }
    }
  }

  function focusThinkingOption(index: number) {
    const nextIndex = wrapIndex(index, levels.length);
    setFocusedIndex(nextIndex);
    itemRefs.current[nextIndex]?.focus();
  }

  if (!current && !hasChoices) {
    return (
      <span
        className="composer-thinking"
        id="composer-thinking"
        aria-live="polite"
      />
    );
  }

  if (!hasChoices) {
    return (
      <span
        className="composer-thinking"
        id="composer-thinking"
        aria-live="polite"
      >
        {formatThinkingLabel(current)}
      </span>
    );
  }

  return (
    <span className="composer-thinking-menu" ref={menuRef}>
      <button
        ref={triggerRef}
        type="button"
        id="composer-thinking"
        className="thinking-trigger"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-live="polite"
        disabled={disabled}
        onClick={() => (open ? closeMenu() : openMenu())}
        onKeyDown={handleTriggerKeyDown}
      >
        <span>{formatThinkingLabel(current)}</span>
        <span className="thinking-chevron" aria-hidden="true">
          v
        </span>
      </button>
      {open ? (
        <div
          className="thinking-menu"
          role="menu"
          onKeyDown={handleMenuKeyDown}
        >
          {levels.map((level, index) => {
            const selected = level === current;
            return (
              <button
                key={level}
                ref={(node) => {
                  itemRefs.current[index] = node;
                }}
                type="button"
                className="thinking-option"
                role="menuitemradio"
                aria-checked={selected ? "true" : "false"}
                tabIndex={index === focusedIndex ? 0 : -1}
                onFocus={() => setFocusedIndex(index)}
                onClick={() => selectThinking(level)}
              >
                <span>
                  <span className="thinking-option-label">
                    {formatThinkingLabel(level)}
                  </span>
                  <span className="thinking-option-description">
                    {thinkingLevelDetails[level] ?? ""}
                  </span>
                </span>
                <span className="thinking-check" aria-hidden="true">
                  {selected ? "✓" : ""}
                </span>
              </button>
            );
          })}
        </div>
      ) : null}
    </span>
  );
}

function ModelMenu({
  disabled,
  modelInfo,
  options,
  onModelChange,
}: {
  disabled: boolean;
  modelInfo: ModelInfo | null;
  options: ModelOption[];
  onModelChange: (option: ModelOption) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const menuRef = useRef<HTMLSpanElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const hasOptions = options.length > 0;
  const visibleOptions = filterModelOptions(options, query);

  const closeMenu = useCallback(() => {
    setOpen(false);
    setQuery("");
    window.setTimeout(() => {
      triggerRef.current?.focus();
    }, 0);
  }, []);

  const openMenu = useCallback(() => {
    setOpen(true);
    window.setTimeout(() => {
      searchRef.current?.focus();
    }, 0);
  }, []);

  useEffect(() => {
    if (!open) {
      return undefined;
    }

    function closeOnOutsidePointer(event: PointerEvent) {
      if (!menuRef.current?.contains(event.target as Node)) {
        closeMenu();
      }
    }

    document.addEventListener("pointerdown", closeOnOutsidePointer);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
    };
  }, [open, closeMenu]);

  useEffect(() => {
    if (disabled || !hasOptions) {
      closeMenu();
    }
  }, [disabled, hasOptions, closeMenu]);

  function selectModel(option: ModelOption) {
    onModelChange(option);
    closeMenu();
  }

  function closeOnEscape(event: React.KeyboardEvent) {
    if (event.key === "Escape") {
      closeMenu();
    }
  }

  function handleSearchKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    event.stopPropagation();
    if (event.key === "Enter") {
      event.preventDefault();
      return;
    }
    closeOnEscape(event);
  }

  function toggleMenu() {
    if (open) {
      closeMenu();
    } else {
      openMenu();
    }
  }

  if (!hasOptions) {
    return (
      <span className="composer-model" id="composer-model" aria-live="polite">
        {modelInfo?.label ?? ""}
      </span>
    );
  }

  return (
    <span className="composer-model-menu" ref={menuRef}>
      <button
        ref={triggerRef}
        type="button"
        id="composer-model"
        className="model-trigger"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-live="polite"
        disabled={disabled}
        onClick={toggleMenu}
        onKeyDown={closeOnEscape}
      >
        <span>{modelInfo?.label ?? "Model"}</span>
        <span className="model-chevron" aria-hidden="true">
          v
        </span>
      </button>
      {open ? (
        <div className="model-menu" role="menu" onKeyDown={closeOnEscape}>
          <input
            ref={searchRef}
            type="search"
            className="model-search"
            aria-label="Search models"
            placeholder="Search models"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={handleSearchKeyDown}
          />
          {visibleOptions.map((option) => {
            const selected = isCurrentModel(option, modelInfo);
            return (
              <button
                key={`${option.provider}:${option.model}`}
                type="button"
                className="model-option"
                role="menuitemradio"
                aria-checked={selected ? "true" : "false"}
                onClick={() => selectModel(option)}
              >
                <span className="model-option-label">{option.label}</span>
                <span className="model-option-provider">{option.provider}</span>
                <span className="model-check" aria-hidden="true">
                  {selected ? "✓" : ""}
                </span>
              </button>
            );
          })}
          {visibleOptions.length === 0 ? (
            <div className="model-empty">No matching models</div>
          ) : null}
        </div>
      ) : null}
    </span>
  );
}

function filterModelOptions(
  options: ModelOption[],
  query: string,
): ModelOption[] {
  const normalizedQuery = query.trim().toLowerCase();
  if (!normalizedQuery) {
    return options;
  }
  return options.filter((option) =>
    modelSearchText(option).includes(normalizedQuery),
  );
}

function modelSearchText(option: ModelOption): string {
  return `${option.label} ${option.provider} ${option.model}`.toLowerCase();
}

function isCurrentModel(option: ModelOption, modelInfo: ModelInfo | null) {
  return (
    option.provider === modelInfo?.provider && option.model === modelInfo?.model
  );
}

function formatThinkingLabel(level: string): string {
  if (!level) {
    return "";
  }
  return level === "off" ? "thinking off" : `thinking ${level}`;
}

function wrapIndex(index: number, length: number): number {
  return ((index % length) + length) % length;
}

function SessionModeMenu({
  disabled,
  mode,
  onModeChange,
}: {
  disabled: boolean;
  mode: SessionMode;
  onModeChange: (mode: SessionMode) => void;
}) {
  const [open, setOpen] = useState(false);
  const menuRef = useRef<HTMLSpanElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const closeMenu = useCallback(() => {
    setOpen(false);
    window.setTimeout(() => {
      triggerRef.current?.focus();
    }, 0);
  }, []);

  useEffect(() => {
    if (!open) {
      return undefined;
    }

    function closeOnOutsidePointer(event: PointerEvent) {
      if (!menuRef.current?.contains(event.target as Node)) {
        closeMenu();
      }
    }

    document.addEventListener("pointerdown", closeOnOutsidePointer);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointer);
    };
  }, [open, closeMenu]);

  useEffect(() => {
    if (disabled) {
      closeMenu();
    }
  }, [disabled, closeMenu]);

  function selectMode(value: SessionMode) {
    onModeChange(value);
    closeMenu();
  }

  function closeOnEscape(event: React.KeyboardEvent) {
    if (event.key === "Escape") {
      closeMenu();
    }
  }

  return (
    <span className="composer-session-mode" ref={menuRef}>
      <button
        ref={triggerRef}
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

function sessionModeLabel(mode: SessionMode): string {
  return (
    sessionModeOptions.find((option) => option.value === mode)?.label ??
    sessionModeOptions[0].label
  );
}

function formatTokenUsage(tokenUsage: TokenUsage | null | undefined): string {
  if (!tokenUsage?.contextWindow) {
    return "";
  }
  const used = Number.isFinite(tokenUsage.used) ? tokenUsage.used : 0;
  return `${tokenFormatter.format(used)}/${tokenFormatter.format(
    tokenUsage.contextWindow,
  )}`;
}
