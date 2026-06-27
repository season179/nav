import { readFile, rename, writeFile } from "node:fs/promises";
import { join } from "node:path";
import {
  type OAuthCredentials,
  refreshOpenAICodexToken,
} from "@earendil-works/pi-ai/oauth";
import { registerProvider } from "@flue/runtime";
import { getCodexHome } from "./codex.js";

const providerId = "openai-codex";
const refreshSkewMs = 5 * 60 * 1000;
const backgroundRefreshMs = 4 * 60 * 1000;
// pi-ai's openai-codex provider reads the ChatGPT account id from this JWT
// claim and throws at request time if it is absent; mirror that requirement so
// we surface an actionable error at readiness time instead.
const jwtAuthClaim = "https://api.openai.com/auth";

let credentials: OAuthCredentials | null = null;
let inflight: Promise<void> | null = null;
let backgroundTimer: ReturnType<typeof setInterval> | null = null;
let registeredAccess: string | null = null;

const reloginHint =
  "Run `codex login` with a ChatGPT subscription account first.";

function getAuthPath(): string {
  return join(getCodexHome(), "auth.json");
}

function decodeJwtPayload(accessToken: string): Record<string, unknown> {
  const segments = accessToken.split(".");
  const payloadSegment = segments[1];

  if (segments.length !== 3 || !payloadSegment) {
    throw new Error(`Codex access_token is not a JWT. ${reloginHint}`);
  }

  try {
    return JSON.parse(
      Buffer.from(payloadSegment, "base64url").toString("utf8"),
    ) as Record<string, unknown>;
  } catch {
    throw new Error(
      `Codex access_token JWT payload is unreadable. ${reloginHint}`,
    );
  }
}

function getJwtExpiresMs(payload: Record<string, unknown>): number {
  if (typeof payload.exp !== "number") {
    throw new Error(
      `Codex access_token JWT has no numeric exp claim. ${reloginHint}`,
    );
  }

  return payload.exp * 1000;
}

function assertChatgptAccountId(payload: Record<string, unknown>): void {
  const auth = payload[jwtAuthClaim];
  const accountId =
    typeof auth === "object" && auth !== null
      ? (auth as { chatgpt_account_id?: unknown }).chatgpt_account_id
      : undefined;

  if (typeof accountId !== "string" || !accountId) {
    throw new Error(
      `Codex access_token has no ChatGPT account id. ${reloginHint}`,
    );
  }
}

async function seedFromAuthFile(): Promise<OAuthCredentials> {
  let raw: string;

  try {
    raw = await readFile(getAuthPath(), "utf8");
  } catch {
    throw new Error(`Codex auth not found at ${getAuthPath()}. ${reloginHint}`);
  }

  let parsed: {
    auth_mode?: unknown;
    tokens?: { access_token?: unknown; refresh_token?: unknown };
  };

  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error(
      `Codex auth.json at ${getAuthPath()} is corrupted. ${reloginHint}`,
    );
  }

  const tokens = parsed.tokens;

  if (
    parsed.auth_mode !== "chatgpt" ||
    typeof tokens?.access_token !== "string" ||
    typeof tokens.refresh_token !== "string"
  ) {
    throw new Error(
      'Codex auth.json is not a ChatGPT subscription login (expected auth_mode "chatgpt" with tokens). Run `codex login`.',
    );
  }

  const payload = decodeJwtPayload(tokens.access_token);
  assertChatgptAccountId(payload);

  return {
    access: tokens.access_token,
    refresh: tokens.refresh_token,
    expires: getJwtExpiresMs(payload),
  };
}

async function persistRefreshed(nextCredentials: OAuthCredentials) {
  const authPath = getAuthPath();
  let current: Record<string, unknown> = {};

  try {
    current = JSON.parse(await readFile(authPath, "utf8")) as Record<
      string,
      unknown
    >;
  } catch {
    current = {};
  }

  const tokens =
    typeof current.tokens === "object" && current.tokens !== null
      ? (current.tokens as Record<string, unknown>)
      : {};

  tokens.access_token = nextCredentials.access;
  tokens.refresh_token = nextCredentials.refresh;

  const merged = {
    ...current,
    auth_mode: "chatgpt",
    last_refresh: new Date().toISOString(),
    tokens,
  };
  const tmp = `${authPath}.nav-${process.pid}.tmp`;

  await writeFile(tmp, `${JSON.stringify(merged, null, 2)}\n`, { mode: 0o600 });
  await rename(tmp, authPath);
}

async function refreshIfNeeded() {
  credentials ??= await seedFromAuthFile();

  if (Date.now() < credentials.expires - refreshSkewMs) {
    return;
  }

  const nextCredentials = await refreshOpenAICodexToken(credentials.refresh);
  credentials = nextCredentials;

  try {
    await persistRefreshed(nextCredentials);
  } catch (error) {
    // The in-memory bearer keeps this process working, but the refresh token
    // was rotated server-side: auth.json now holds a consumed token, so the
    // next cold start may need a re-login. Surface it instead of swallowing.
    console.error(
      "[nav] Failed to persist refreshed Codex tokens; a restart may require `codex login`:",
      error instanceof Error ? error.message : error,
    );
  }
}

export async function ensureCodexProvider(): Promise<void> {
  inflight ??= (async () => {
    try {
      await refreshIfNeeded();

      if (!credentials) {
        throw new Error("Codex credentials unavailable.");
      }

      // Re-register only when the bearer actually changed. refreshIfNeeded is a
      // cheap no-op once the token is seeded and not near expiry, so steady-state
      // requests skip the provider registration entirely.
      if (credentials.access !== registeredAccess) {
        registerProvider(providerId, { apiKey: credentials.access });
        registeredAccess = credentials.access;
      }
    } finally {
      inflight = null;
    }
  })();

  return await inflight;
}

export function startCodexProviderRefresh(): void {
  if (backgroundTimer) {
    return;
  }

  backgroundTimer = setInterval(() => {
    void ensureCodexProvider().catch(() => undefined);
  }, backgroundRefreshMs);
  backgroundTimer.unref?.();
}
