import type { ModelInfo, ModelOption, SessionMode } from "../types.ts";

export type SettingsFormValues = {
  mode: SessionMode;
  modelKey: string;
  thinking: string;
};

export const settingsSessionModeOptions: {
  value: SessionMode;
  label: string;
}[] = [
  { value: "local", label: "Local" },
  { value: "worktree", label: "Worktree" },
];

export function settingsFormDefaults(
  mode: SessionMode,
  modelInfo: ModelInfo | null,
): SettingsFormValues {
  const thinkingLevels = thinkingLevelsFor(modelInfo);
  return {
    mode,
    modelKey: modelInfoKey(modelInfo),
    thinking: modelInfo?.thinking ?? thinkingLevels[0] ?? "",
  };
}

export function sessionModeLabel(mode: SessionMode): string {
  return (
    settingsSessionModeOptions.find((option) => option.value === mode)?.label ??
    settingsSessionModeOptions[0].label
  );
}

export function modelOptionKey(option: ModelOption): string {
  return `${option.provider}:${option.model}`;
}

export function modelInfoKey(modelInfo: ModelInfo | null): string {
  if (!modelInfo?.provider || !modelInfo.model) {
    return "";
  }
  return `${modelInfo.provider}:${modelInfo.model}`;
}

export function modelOptionSearchText(option: ModelOption): string {
  return `${option.label} ${option.provider} ${option.model}`.toLowerCase();
}

export function modelOptionMatchesQuery(
  option: ModelOption,
  query: string,
): boolean {
  const normalizedQuery = query.trim().toLowerCase();
  return (
    normalizedQuery.length === 0 ||
    modelOptionSearchText(option).includes(normalizedQuery)
  );
}

export function thinkingLevelsFor(modelInfo: ModelInfo | null): string[] {
  const levels = modelInfo?.thinkingLevels;
  return Array.isArray(levels) ? levels : [];
}

export function formatThinkingLabel(level: string): string {
  if (!level) {
    return "";
  }
  return level === "off" ? "thinking off" : `thinking ${level}`;
}
