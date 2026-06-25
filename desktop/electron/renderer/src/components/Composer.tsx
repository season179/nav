import { useForm } from "@tanstack/react-form";
import { useDebouncer } from "@tanstack/react-pacer";
import type { FormEvent } from "react";
import { useEffect, useRef, useState } from "react";
import {
  ContextContent,
  ContextContentHeader,
  ContextTrigger,
  Context as TokenContext,
} from "@/components/ai-elements/context";
import {
  ModelSelector,
  ModelSelectorContent,
  ModelSelectorEmpty,
  ModelSelectorGroup,
  ModelSelectorInput,
  ModelSelectorItem,
  ModelSelectorList,
  ModelSelectorName,
  ModelSelectorShortcut,
  ModelSelectorTrigger,
} from "@/components/ai-elements/model-selector";
import {
  PromptInput,
  PromptInputBody,
  PromptInputButton,
  PromptInputFooter,
  type PromptInputMessage,
  PromptInputSubmit,
  PromptInputTextarea,
  PromptInputTools,
} from "@/components/ai-elements/prompt-input";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  browserComposerDraftStorage,
  readComposerDraft,
  writeComposerDraft,
} from "../lib/composer-draft.ts";
import {
  type ComposerFormValues,
  normalizeComposerMessage,
  validateComposerMessage,
} from "../lib/composer-validation.ts";
import type {
  ModelInfo,
  ModelOption,
  SessionMode,
  TokenUsage,
} from "../types.ts";

const sessionModeOptions: { value: SessionMode; label: string }[] = [
  { value: "local", label: "Local" },
  { value: "worktree", label: "Worktree" },
];
export default function Composer({
  connected,
  draftKey,
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
  draftKey: string | null;
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
  const draftStorage = browserComposerDraftStorage();
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const saveDraftDebouncer = useDebouncer(
    (draft: string) => writeComposerDraft(draftStorage, draftKey, draft),
    {
      onUnmount: (debouncer) => debouncer.flush(),
      wait: 250,
    },
  );
  const form = useForm({
    defaultValues: {
      message: readComposerDraft(draftStorage, draftKey),
    } satisfies ComposerFormValues,
    onSubmit: async ({ formApi, value }) => {
      const message = normalizeComposerMessage(value.message);
      if (!message || !connected) {
        return;
      }

      saveDraftDebouncer.cancel();
      writeComposerDraft(draftStorage, draftKey, "");
      formApi.reset({ message: "" });
      await onSend(message);
    },
  });

  useEffect(() => {
    if (connected) {
      inputRef.current?.focus();
    }
  }, [connected]);

  function saveDraft(nextText: string) {
    saveDraftDebouncer.maybeExecute(nextText);
  }

  async function handlePromptSubmit(
    _message: PromptInputMessage,
    event: FormEvent<HTMLFormElement>,
  ) {
    event.stopPropagation();
    await form.handleSubmit();
  }

  return (
    <div className="composer">
      <form.Field
        name="message"
        validators={{
          onSubmit: ({ value }) => validateComposerMessage(value, connected),
        }}
      >
        {(field) => {
          const message = field.state.value;
          const errorText = field.state.meta.errors.join(", ");
          return (
            <>
              <PromptInput
                className="composer-input-form"
                id="composer"
                onSubmit={handlePromptSubmit}
              >
                <PromptInputBody>
                  <PromptInputTextarea
                    ref={inputRef}
                    id="composer-input"
                    className="composer-input"
                    aria-label="Message"
                    aria-describedby={
                      errorText ? "composer-input-error" : undefined
                    }
                    aria-invalid={errorText ? true : undefined}
                    placeholder="Tell nav what to do"
                    autoComplete="off"
                    rows={1}
                    disabled={!connected}
                    value={message}
                    onBlur={field.handleBlur}
                    onChange={(event) => {
                      const nextMessage = event.target.value;
                      field.handleChange(nextMessage);
                      saveDraft(nextMessage);
                    }}
                  />
                </PromptInputBody>
                <PromptInputFooter className="composer-input-footer">
                  <PromptInputTools>
                    {running ? (
                      <PromptInputButton
                        type="button"
                        id="composer-stop"
                        className="composer-stop"
                        disabled={!connected || stopPending}
                        onClick={onStop}
                      >
                        Stop
                      </PromptInputButton>
                    ) : null}
                  </PromptInputTools>
                  <PromptInputSubmit
                    id="composer-send"
                    className="composer-send"
                    aria-label="Send message"
                    title="Send message"
                    disabled={
                      !connected ||
                      normalizeComposerMessage(message).length === 0
                    }
                  >
                    ↑
                  </PromptInputSubmit>
                </PromptInputFooter>
              </PromptInput>
              {errorText ? (
                <div
                  className="composer-error"
                  id="composer-input-error"
                  role="alert"
                >
                  {errorText}
                </div>
              ) : null}
            </>
          );
        }}
      </form.Field>
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
          <TokenUsageMeter modelInfo={modelInfo} />
        </span>
      </div>
    </div>
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
  const rawLevels = modelInfo?.thinkingLevels;
  const levels = Array.isArray(rawLevels) ? rawLevels : [];
  const current = modelInfo?.thinking ?? levels[0] ?? "";
  const hasChoices = levels.length > 1;

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
    <Select
      value={current}
      disabled={disabled || !hasChoices}
      onValueChange={onThinkingChange}
    >
      <SelectTrigger
        id="composer-thinking"
        className="thinking-trigger"
        aria-label="Thinking level"
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent align="end" className="composer-select-content">
        <SelectGroup>
          {levels.map((level) => (
            <SelectItem key={level} value={level}>
              {formatThinkingLabel(level)}
            </SelectItem>
          ))}
        </SelectGroup>
      </SelectContent>
    </Select>
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
  const hasOptions = options.length > 0;
  const groupedOptions = groupModelOptions(options);

  useEffect(() => {
    if (disabled || !hasOptions) {
      setOpen(false);
    }
  }, [disabled, hasOptions]);

  function selectModel(option: ModelOption) {
    onModelChange(option);
    setOpen(false);
  }

  if (!hasOptions) {
    return (
      <span className="composer-model" id="composer-model" aria-live="polite">
        {modelInfo?.label ?? ""}
      </span>
    );
  }

  return (
    <ModelSelector
      open={open}
      onOpenChange={(nextOpen) => {
        setOpen(disabled ? false : nextOpen);
      }}
    >
      <ModelSelectorTrigger asChild>
        <button
          type="button"
          id="composer-model"
          className="model-trigger"
          aria-live="polite"
          disabled={disabled}
        >
          <span>{modelInfo?.label ?? "Model"}</span>
          <span className="model-chevron" aria-hidden="true">
            v
          </span>
        </button>
      </ModelSelectorTrigger>
      <ModelSelectorContent className="model-selector-content">
        <ModelSelectorInput placeholder="Search models" />
        <ModelSelectorList>
          <ModelSelectorEmpty>No matching models</ModelSelectorEmpty>
          {groupedOptions.map(([provider, providerOptions]) => (
            <ModelSelectorGroup heading={provider} key={provider}>
              {providerOptions.map((option) => {
                const selected = isCurrentModel(option, modelInfo);
                return (
                  <ModelSelectorItem
                    key={`${option.provider}:${option.model}`}
                    className="model-option"
                    data-current={selected ? "true" : undefined}
                    value={modelSearchText(option)}
                    onSelect={() => selectModel(option)}
                  >
                    <ModelSelectorName>{option.label}</ModelSelectorName>
                    <span className="model-option-provider">
                      {option.provider}
                    </span>
                    <ModelSelectorShortcut>
                      {selected ? "✓" : ""}
                    </ModelSelectorShortcut>
                  </ModelSelectorItem>
                );
              })}
            </ModelSelectorGroup>
          ))}
        </ModelSelectorList>
      </ModelSelectorContent>
    </ModelSelector>
  );
}

function groupModelOptions(options: ModelOption[]): [string, ModelOption[]][] {
  const groups = new Map<string, ModelOption[]>();
  for (const option of options) {
    const provider = option.provider || "Other";
    const providerOptions = groups.get(provider);
    if (providerOptions) {
      providerOptions.push(option);
    } else {
      groups.set(provider, [option]);
    }
  }
  return [...groups.entries()];
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

function SessionModeMenu({
  disabled,
  mode,
  onModeChange,
}: {
  disabled: boolean;
  mode: SessionMode;
  onModeChange: (mode: SessionMode) => void;
}) {
  return (
    <Select
      value={mode}
      disabled={disabled}
      onValueChange={(value) => {
        if (value === "local" || value === "worktree") {
          onModeChange(value);
        }
      }}
    >
      <SelectTrigger
        id="new-session-mode"
        className="session-mode-trigger"
        aria-label="New session mode"
      >
        <SelectValue />
      </SelectTrigger>
      <SelectContent align="start" className="composer-select-content">
        <SelectGroup>
          {sessionModeOptions.map((option) => (
            <SelectItem key={option.value} value={option.value}>
              {option.label}
            </SelectItem>
          ))}
        </SelectGroup>
      </SelectContent>
    </Select>
  );
}

function TokenUsageMeter({ modelInfo }: { modelInfo: ModelInfo | null }) {
  const tokenUsage = modelInfo?.tokenUsage;
  if (!tokenUsage?.contextWindow) {
    return <span className="composer-token-usage" id="composer-token-usage" />;
  }

  const usedTokens = Math.max(0, tokenUsage.used);
  const maxTokens = Math.max(1, tokenUsage.contextWindow);

  return (
    <TokenContext usedTokens={usedTokens} maxTokens={maxTokens}>
      <ContextTrigger>
        <button
          type="button"
          className="composer-token-usage"
          id="composer-token-usage"
        >
          {formatTokenUsage(tokenUsage)}
        </button>
      </ContextTrigger>
      <ContextContent align="end">
        <ContextContentHeader />
      </ContextContent>
    </TokenContext>
  );
}

function formatTokenUsage(tokenUsage: TokenUsage | null | undefined): string {
  if (!tokenUsage?.contextWindow) {
    return "";
  }
  return `${formatTokenCount(tokenUsage.used)}/${formatTokenCount(
    tokenUsage.contextWindow,
  )}`;
}

function formatTokenCount(value: number): string {
  if (!Number.isFinite(value) || value < 1000) {
    return "0";
  }

  if (value >= 1_000_000) {
    return `${Math.floor(value / 1_000_000)}M`;
  }

  return `${Math.floor(value / 1000)}k`;
}
