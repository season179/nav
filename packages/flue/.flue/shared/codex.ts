import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { defineTool } from "@flue/runtime";
import { Codex, type ThreadOptions, type Usage } from "@openai/codex-sdk";
import * as v from "valibot";

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

export const codexTaskInputSchema = v.object({
  prompt: v.pipe(
    v.string(),
    v.minLength(1),
    v.description("The bounded coding-agent task to run through Codex."),
  ),
  threadId: v.optional(
    v.pipe(
      v.string(),
      v.minLength(1),
      v.description("Existing Codex thread ID to resume."),
    ),
  ),
});

type CodexTaskInput = v.InferOutput<typeof codexTaskInputSchema>;

export type CodexTaskResult = {
  ok: boolean;
  auth: CodexAuthStatus;
  threadId: string | null;
  finalResponse: string | null;
  usage: Usage | null;
  error: string | null;
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

export async function runCodexTask(
  input: CodexTaskInput,
  signal?: AbortSignal,
): Promise<CodexTaskResult> {
  const auth = await getCodexAuthStatus();

  if (auth.status !== "ready") {
    return {
      ok: false,
      auth,
      threadId: null,
      finalResponse: null,
      usage: null,
      error: auth.message,
    };
  }

  try {
    const codex = new Codex();
    const threadOptions = getThreadOptions();
    const thread = input.threadId
      ? codex.resumeThread(input.threadId, threadOptions)
      : codex.startThread(threadOptions);
    const result = await thread.run(input.prompt, { signal });

    return {
      ok: true,
      auth,
      threadId: thread.id,
      finalResponse: result.finalResponse,
      usage: result.usage,
      error: null,
    };
  } catch (error) {
    return {
      ok: false,
      auth,
      threadId: input.threadId ?? null,
      finalResponse: null,
      usage: null,
      error: error instanceof Error ? error.message : "Codex run failed.",
    };
  }
}

export const runCodexTaskTool = defineTool({
  name: "run_codex_task",
  description:
    "Run a bounded Codex local task using the user's Codex ChatGPT subscription auth or CODEX_ACCESS_TOKEN.",
  input: codexTaskInputSchema,
  async run({ input, signal }) {
    return await runCodexTask(input, signal);
  },
});

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

function getCodexHome(): string {
  const configured = process.env.CODEX_HOME?.trim();
  return configured ? configured : join(homedir(), ".codex");
}

function getThreadOptions(): ThreadOptions {
  return {
    approvalPolicy: "never",
    model: process.env.NAV_CODEX_MODEL?.trim() || undefined,
    sandboxMode: "read-only",
    webSearchMode: "disabled",
    workingDirectory: getWorkspaceRoot(),
  };
}

function getWorkspaceRoot(): string {
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
