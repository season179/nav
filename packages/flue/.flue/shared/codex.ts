import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";

type CodexAuthMode = "access_token" | "chatgpt" | "api_key";

export type CodexAuthStatus = {
  status: "ready" | "missing_auth";
  mode: CodexAuthMode | null;
  subscriptionBacked: boolean;
  source: "CODEX_ACCESS_TOKEN" | "codex_auth_cache" | null;
  message: string;
};

type AuthCache = {
  auth_mode?: unknown;
  tokens?: unknown;
  OPENAI_API_KEY?: unknown;
};

type TokenCache = {
  access_token?: unknown;
  refresh_token?: unknown;
};

export async function getCodexAuthStatus(): Promise<CodexAuthStatus> {
  if (process.env.CODEX_ACCESS_TOKEN?.trim()) {
    return {
      status: "ready",
      mode: "access_token",
      subscriptionBacked: true,
      source: "CODEX_ACCESS_TOKEN",
      message: "Codex access token is configured for ChatGPT workspace auth.",
    };
  }

  const authCache = await readAuthCache();

  if (!authCache) {
    return {
      status: "missing_auth",
      mode: null,
      subscriptionBacked: false,
      source: null,
      message:
        "Codex is not authenticated. Run `codex login` for ChatGPT subscription access, or set `CODEX_ACCESS_TOKEN` for trusted Business/Enterprise automation.",
    };
  }

  const authMode =
    typeof authCache.auth_mode === "string" ? authCache.auth_mode : null;
  const tokens = isRecord(authCache.tokens)
    ? (authCache.tokens as TokenCache)
    : null;

  if (
    authMode === "chatgpt" &&
    typeof tokens?.access_token === "string" &&
    typeof tokens.refresh_token === "string"
  ) {
    return {
      status: "ready",
      mode: "chatgpt",
      subscriptionBacked: true,
      source: "codex_auth_cache",
      message: "Codex ChatGPT login is available from the local auth cache.",
    };
  }

  if (
    (authMode === "api" || authMode === "api_key" || authMode === "apikey") &&
    authCache.OPENAI_API_KEY != null
  ) {
    return {
      status: "ready",
      mode: "api_key",
      subscriptionBacked: false,
      source: "codex_auth_cache",
      message:
        "Codex API-key auth is available. This is not ChatGPT subscription-backed.",
    };
  }

  return {
    status: "missing_auth",
    mode: null,
    subscriptionBacked: false,
    source: null,
    message:
      "Codex auth cache exists, but no usable ChatGPT, access-token, or API-key credential was detected.",
  };
}

async function readAuthCache(): Promise<AuthCache | null> {
  const authPath = join(getCodexHome(), "auth.json");

  try {
    const raw = await readFile(authPath, "utf8");
    const parsed: unknown = JSON.parse(raw);
    return isRecord(parsed) ? (parsed as AuthCache) : null;
  } catch {
    return null;
  }
}

export function getCodexHome(): string {
  const configured = process.env.CODEX_HOME?.trim();
  return configured ? configured : join(homedir(), ".codex");
}

export function getWorkspaceRoot(): string {
  const configured = process.env.NAV_CODEX_WORKDIR?.trim();
  if (configured) return resolve(configured);

  let cursor = resolve(process.cwd());

  while (true) {
    if (existsSync(join(cursor, "pnpm-workspace.yaml"))) return cursor;

    const next = dirname(cursor);
    if (next === cursor) return resolve(process.cwd());
    cursor = next;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
