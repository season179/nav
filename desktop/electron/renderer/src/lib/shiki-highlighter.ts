import type {
  BundledLanguage,
  HighlighterGeneric,
  LanguageRegistration,
  ThemedToken,
  ThemeRegistrationAny,
} from "shiki";
import { createHighlighterCore } from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";
import type { CodeHighlighterPlugin, ThemeInput } from "streamdown";

export interface TokenizedCode {
  bg: string;
  fg: string;
  rootStyle?: string | false;
  tokens: ThemedToken[][];
}

type LanguageLoader = () => Promise<{
  default: LanguageRegistration[];
}>;

const languageLoaders = {
  bash: () => import("shiki/dist/langs/bash.mjs"),
  css: () => import("shiki/dist/langs/css.mjs"),
  diff: () => import("shiki/dist/langs/diff.mjs"),
  go: () => import("shiki/dist/langs/go.mjs"),
  html: () => import("shiki/dist/langs/html.mjs"),
  javascript: () => import("shiki/dist/langs/javascript.mjs"),
  json: () => import("shiki/dist/langs/json.mjs"),
  jsx: () => import("shiki/dist/langs/jsx.mjs"),
  markdown: () => import("shiki/dist/langs/markdown.mjs"),
  python: () => import("shiki/dist/langs/python.mjs"),
  rust: () => import("shiki/dist/langs/rust.mjs"),
  shellscript: () => import("shiki/dist/langs/shellscript.mjs"),
  tsx: () => import("shiki/dist/langs/tsx.mjs"),
  typescript: () => import("shiki/dist/langs/typescript.mjs"),
} satisfies Record<string, LanguageLoader>;

type SupportedLanguage = keyof typeof languageLoaders;

const supportedLanguages = Object.keys(languageLoaders) as SupportedLanguage[];

const languageAliases: Record<string, SupportedLanguage> = {
  bash: "bash",
  css: "css",
  diff: "diff",
  go: "go",
  html: "html",
  javascript: "javascript",
  js: "javascript",
  json: "json",
  jsx: "jsx",
  markdown: "markdown",
  md: "markdown",
  py: "python",
  python: "python",
  rs: "rust",
  rust: "rust",
  sh: "shellscript",
  shell: "shellscript",
  shellscript: "shellscript",
  ts: "typescript",
  tsx: "tsx",
  typescript: "typescript",
  zsh: "shellscript",
};

const themes = ["github-light", "github-dark"] as const;
type HighlightTheme = (typeof themes)[number];
type NavHighlighter = HighlighterGeneric<SupportedLanguage, HighlightTheme>;
type ThemeLoader = () => Promise<{
  default: ThemeRegistrationAny;
}>;

const themeLoaders: [ThemeLoader, ThemeLoader] = [
  () => import("shiki/dist/themes/github-light.mjs"),
  () => import("shiki/dist/themes/github-dark.mjs"),
];

const highlighterCache = new Map<SupportedLanguage, Promise<NavHighlighter>>();
const tokensCache = new Map<string, TokenizedCode>();
const pendingHighlights = new Map<string, Promise<void>>();
const subscribers = new Map<string, Set<(result: TokenizedCode) => void>>();

const engine = createJavaScriptRegexEngine({ forgiving: true });

const normalizeLanguage = (language: string): SupportedLanguage | null => {
  const normalized = language.trim().toLowerCase();
  return languageAliases[normalized] ?? null;
};

export const createRawTokens = (code: string): TokenizedCode => ({
  bg: "transparent",
  fg: "inherit",
  tokens: code.split("\n").map((line) =>
    line === ""
      ? []
      : [
          {
            color: "inherit",
            content: line,
          } as ThemedToken,
        ],
  ),
});

export const supportsHighlightLanguage = (language: string) =>
  normalizeLanguage(language) !== null;

const getTokensCacheKey = (code: string, language: string) => {
  const start = code.slice(0, 100);
  const end = code.length > 100 ? code.slice(-100) : "";
  return `${language}:${code.length}:${start}:${end}`;
};

const getHighlighter = (language: SupportedLanguage) => {
  const cached = highlighterCache.get(language);
  if (cached) {
    return cached;
  }

  const highlighterPromise = createHighlighterCore({
    engine,
    langs: [languageLoaders[language]],
    themes: themeLoaders,
    warnings: false,
  }).then((highlighter) => highlighter as NavHighlighter);

  highlighterCache.set(language, highlighterPromise);
  return highlighterPromise;
};

const notifySubscribers = (key: string, result: TokenizedCode) => {
  const subs = subscribers.get(key);
  if (!subs) {
    return;
  }

  for (const sub of subs) {
    sub(result);
  }

  subscribers.delete(key);
};

export const highlightCode = (
  code: string,
  language: string,
  callback?: (result: TokenizedCode) => void,
): TokenizedCode | null => {
  const normalizedLanguage = normalizeLanguage(language);
  if (!normalizedLanguage) {
    return createRawTokens(code);
  }

  const tokensCacheKey = getTokensCacheKey(code, normalizedLanguage);
  const cached = tokensCache.get(tokensCacheKey);
  if (cached) {
    return cached;
  }

  if (callback) {
    if (!subscribers.has(tokensCacheKey)) {
      subscribers.set(tokensCacheKey, new Set());
    }
    subscribers.get(tokensCacheKey)?.add(callback);
  }

  if (!pendingHighlights.has(tokensCacheKey)) {
    const highlightPromise = getHighlighter(normalizedLanguage)
      .then((highlighter) => {
        const result = highlighter.codeToTokens(code, {
          lang: normalizedLanguage,
          themes: {
            dark: themes[1],
            light: themes[0],
          },
        });

        const tokenized: TokenizedCode = {
          bg: result.bg ?? "transparent",
          fg: result.fg ?? "inherit",
          rootStyle: result.rootStyle,
          tokens: result.tokens,
        };

        tokensCache.set(tokensCacheKey, tokenized);
        notifySubscribers(tokensCacheKey, tokenized);
      })
      .catch((error) => {
        console.error("Failed to highlight code:", error);
        const raw = createRawTokens(code);
        tokensCache.set(tokensCacheKey, raw);
        notifySubscribers(tokensCacheKey, raw);
      })
      .finally(() => {
        pendingHighlights.delete(tokensCacheKey);
      });

    pendingHighlights.set(tokensCacheKey, highlightPromise);
  }

  return null;
};

export const navCodePlugin: CodeHighlighterPlugin = {
  getSupportedLanguages: () => [...supportedLanguages] as BundledLanguage[],
  getThemes: () => [...themes] as [ThemeInput, ThemeInput],
  highlight: ({ code, language }, callback) =>
    highlightCode(code, language, callback),
  name: "shiki",
  supportsLanguage: supportsHighlightLanguage,
  type: "code-highlighter",
};
