export type TitleTranscriptMessage = {
  role: "assistant" | "user";
  text: string;
};

const MAX_TITLE_LENGTH = 80;
const MAX_TITLE_WORDS = 6;

export const TITLE_MODEL = "deepseek/deepseek-v4-flash";

export const isTitleSourceEligible = (source: string) =>
  source === "first-message" || source === "imported";

export const hasTitleTranscriptExchange = (
  transcript: TitleTranscriptMessage[],
) =>
  transcript.some((message) => message.role === "user") &&
  transcript.some((message) => message.role === "assistant");

export const normalizeGeneratedTitle = (value: unknown) => {
  if (typeof value !== "string") {
    return null;
  }

  const title = value
    .replace(/[\r\n]+/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/^["'`]+|["'`.!?:;]+$/g, "");

  if (!title) {
    return null;
  }

  const words = title.split(/\s+/).slice(0, MAX_TITLE_WORDS);
  const normalized = words.join(" ").slice(0, MAX_TITLE_LENGTH).trim();

  return normalized || null;
};

export const buildTitlePrompt = (transcript: TitleTranscriptMessage[]) =>
  [
    "Generate a short title for this chat.",
    "Rules: at most 6 words, no quotes, no punctuation at the end, no preamble.",
    "",
    "Transcript:",
    ...transcript.map(
      (message) =>
        `${message.role === "user" ? "User" : "Assistant"}: ${message.text}`,
    ),
  ].join("\n");
