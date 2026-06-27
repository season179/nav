# Plan: Codex-subscription-backed Nav agent + chat adapter

**Status:** Converged. Architecture and every mechanic below were debated and agreed
between Claude (coordinator) and Codex, and verified against the *installed* source of
`@flue/runtime@1.0.0-beta.7` and `@earendil-works/pi-ai@0.79.10`. An executor should be
able to apply this end-to-end without further research.

---

## 1. Objective

Make **Nav** a real Flue **agent** whose "brain" is the user's **ChatGPT/Codex
subscription** model **`gpt-5.5`**, and wire the desktop chat UI to it through Flue's
first-class agent chat hook (`useFlueAgent`).

Two things are explicitly **wrong** in the current code and are being replaced:

1. The chat runs through a one-shot **workflow** (`workflows/codex-plan.ts` +
   `client.workflows.run("codex-plan", …)`). A workflow is a finite operation, not a
   conversation. **Delete it.**
2. The agent's model is `openai/gpt-5.5` — a *separate, API-key-billed* OpenAI model,
   **not** the subscription. **Replace it** with `openai-codex/gpt-5.5`, served over the
   user's ChatGPT/Codex OAuth via pi-ai's `openai-codex-responses` provider
   (`https://chatgpt.com/backend-api`).

### Architecture (Path B "Hybrid")

- **Brain = the subscription model.** Nav is `defineAgent({ model: 'openai-codex/gpt-5.5' })`.
  The `openai-codex` provider is registered at runtime with a **bearer derived from the
  Codex OAuth credentials** in `~/.codex/auth.json`, refreshed as needed.
- **Tools = a local sandbox.** `sandbox: local({ cwd: repoRoot })` gives Nav built-in
  file + shell capabilities scoped to the repo (per Flue docs: `local()` "makes host
  files and installed commands reachable through the agent's workspace capabilities"). No
  tools need to be declared manually.
- **Hybrid (optional, see §8).** The existing `@openai/codex-sdk`-based
  `run_codex_task` tool can be re-attached to delegate heavy autonomous runs. It is
  **not** part of the core deliverable (the sandbox already lets Nav read/run things), and
  is documented separately so the executor is not blocked.

---

## 2. Verified facts (why each step is safe)

All confirmed by reading installed `node_modules` source on this machine.

| # | Fact | Evidence |
|---|------|----------|
| F1 | `gpt-5.5` **is** in the bundled pi-ai catalog under provider `openai-codex`. | `MODELS["openai-codex"]` = `['gpt-5.3-codex-spark','gpt-5.4','gpt-5.4-mini','gpt-5.5']` |
| F2 | The catalog `gpt-5.5` entry has `reasoning: true`, `thinkingLevelMap: {xhigh:'xhigh', minimal:'low'}`, `api:'openai-codex-responses'`, `baseUrl:'https://chatgpt.com/backend-api'`, `contextWindow:272000`, `maxTokens:128000`, `input:['text','image']`. | `models.generated.js` |
| F3 | `registerProvider('openai-codex', { apiKey })` **preserves the catalog model**. Flue's `buildModelFromRegistration` does `base = getModel(providerId, modelId) ?? zeroMetadataModel(...)` then `return { ...base, api, provider, baseUrl, headers, contextWindow:…, maxTokens:… }`. With only `apiKey` supplied, `base` is the full catalog `gpt-5.5`, so `reasoning:true` + `thinkingLevelMap` + api + baseUrl all carry through. **Do not pass `models`/`headers`/`contextWindow`** — unnecessary and only risks divergence. | `dist/providers-DGTSSRtA.mjs` |
| F4 | Because `reasoning:true`, `getSupportedThinkingLevels` returns the full ladder. The chosen default `'xhigh'` is an explicit key in gpt-5.5's `thinkingLevelMap` (`xhigh→'xhigh'`), so `clampThinkingLevel('xhigh')` returns `'xhigh'` and the provider sends `reasoning_effort:'xhigh'` directly — the lowest-risk effort value. (`'high'`/`'medium'` also work: unmapped levels pass through as-is, and gpt-5.5's documented efforts are `none/low/medium/high/xhigh`.) | `models.js` lines 34-65; `openai-codex-responses.js` line ~350 |
| F5 | The provider extracts `chatgpt_account_id` from the JWT bearer at call time (`extractAccountId`/`getAccountId`), and sets `originator`/`OpenAI-Beta` internally. **No custom headers needed.** | `openai-codex-responses.js`; `oauth/openai-codex.js` `getAccountId` |
| F6 | `openai-codex-responses` is **auto-registered** at import (`registerBuiltInApiProviders()` runs at module top-level). Importing `@flue/runtime` is sufficient. | pi-ai `register-builtins.js` |
| F7 | OAuth helpers are at subpath **`@earendil-works/pi-ai/oauth`**: `refreshOpenAICodexToken(refresh): Promise<OAuthCredentials>`, `openaiCodexOAuthProvider` (`id:'openai-codex'`, `getApiKey(c)=>c.access`), type `OAuthCredentials = { access, refresh, expires, [k]:unknown }`. | `dist/oauth.d.ts`, `dist/utils/oauth/*` |
| F8 | `OAuthCredentials.expires` is **epoch milliseconds**. Refresh sets `expires = Date.now() + expires_in*1000`. The Codex JWT `exp` claim is **seconds**, so seed with `expires = exp * 1000`. | `oauth/openai-codex.js` line 109 |
| F9 | `getOAuthApiKey` refreshes only when **already expired** (`Date.now() >= creds.expires`, no skew). We add our own 5-min skew + single-flight. | `oauth/index.js` line 111 |
| F10 | `getRegisteredApiKey` reads `providersById` at model-call time, so **re-registering rotates the bearer**. | `dist/providers-DGTSSRtA.mjs` |
| F11 | `~/.codex/auth.json` on this machine: top-level `auth_mode:"chatgpt"`, `OPENAI_API_KEY:null`, `last_refresh`, `tokens:{access_token(JWT,~2.1KB), account_id, id_token, refresh_token}`. JWT carries `https://api.openai.com/auth.chatgpt_account_id`. Current token TTL ≈ 42h. | direct read |
| F12 | `db.ts` at the **source root** (`.flue/db.ts`) is auto-discovered (`discoverOptionalEntry(sourceRoot,"db")`); `flue.config.ts` has **no** `store` field. The store is optional (defaults to `:memory:`). | Flue CLI; `reference/configuration` |
| F13 | `sqlite(path)` (from `@flue/runtime/node`) **creates parent dirs** (`mkdirSync(dirname(path),{recursive:true})`), uses `node:sqlite` `DatabaseSync` + WAL. `node:sqlite` works **without a flag** on the repo's Node v24.16.0. | `dist/node/index.mjs` line 67 |
| F14 | `defineAgent(() => AgentRuntimeConfig)` accepts `model`, `instructions`, `tools`, `thinkingLevel` (harness default `'medium'`), `cwd`, `sandbox`. `ModelConfig = string \| false`. | `dist/action-D97NPlzN.d.mts` |
| F15 | `useFlueAgent({ name, id?, history?, live?, client? })` → `{ messages: UIMessage[], status:'idle'\|'connecting'\|'submitted'\|'streaming'\|'error', historyReady, error?: Error, sendMessage(text, opts?) }`. Reads its client from `FlueProvider` context. | `@flue/react` `dist/index.d.mts` |
| F16 | Desktop `connection.baseUrl` already includes `/api` (`http://127.0.0.1:<port>/api`), so the SDK hits `…/api/agents/nav/<id>`, which is covered by the existing CORS + `requireDesktopAuth` on `/api/agents/*`. | `flue-server.ts` |
| F17 | Attaching `sandbox` auto-provides built-in file/command tools (no manual tool list). `local()` is **not** an isolation boundary; host env is limited by default. | Flue `guide/sandboxes` |

---

## 3. Files at a glance

| Action | Path |
|--------|------|
| **add dep** | `packages/flue/package.json` — add `@earendil-works/pi-ai@0.79.10` |
| **new** | `packages/flue/.flue/db.ts` — durable sqlite session store |
| **new** | `packages/flue/.flue/shared/codex-provider.ts` — OAuth read/refresh/register |
| **rewrite** | `packages/flue/.flue/agents/nav.ts` — `openai-codex/gpt-5.5` + local sandbox |
| **rewrite** | `packages/flue/.flue/app.ts` — wire `ensureCodexProvider` (boot + middleware + interval) |
| **1-line edit** | `packages/flue/.flue/shared/codex.ts` — `export` `getWorkspaceRoot` |
| **delete** | `packages/flue/.flue/workflows/codex-plan.ts` (and empty `workflows/` dir) |
| **rewrite** | `packages/desktop/src/main.tsx` — `NavChat` → `useFlueAgent` |
| **edit** | `.gitignore` — ignore `packages/flue/data/` |

---

## 4. Step-by-step

### Step 1 — Add the pi-ai dependency (exact pin)

`@earendil-works/pi-ai` is currently only a *transitive* dep of `@flue/runtime`, so it is
**not resolvable** from `packages/flue` directly. We import its `/oauth` helpers, so add it
as a direct dep, pinned to the same version Flue uses (avoids a duplicate copy):

```bash
pnpm --filter @nav/flue add @earendil-works/pi-ai@0.79.10
```

Resulting `packages/flue/package.json` `dependencies` (note ordering — `@earendil-works`
sorts before `@flue`):

```jsonc
"dependencies": {
  "@earendil-works/pi-ai": "0.79.10",
  "@flue/runtime": "1.0.0-beta.7",
  "@openai/codex-sdk": "0.142.2",
  "hono": "4.12.27",
  "valibot": "1.4.1"
},
```

> If `pnpm add` resolves a newer pi-ai, force `0.79.10` (the version Flue beta.7 pins) so
> there is a single instance. Then `pnpm install`.

### Step 2 — Create the durable store: `packages/flue/.flue/db.ts`

```ts
import { sqlite } from "@flue/runtime/node";

// Auto-discovered by the Flue CLI at the source root (.flue/db.ts). Gives agents a
// durable session store so chat history replays across dev-server restarts.
// sqlite() creates the parent dir and enables WAL; path is relative to the flue
// package cwd. Omit/`:memory:` would make history non-durable.
export default sqlite("./data/flue.db");
```

### Step 3 — Create the OAuth provider bridge: `packages/flue/.flue/shared/codex-provider.ts`

```ts
import { readFile, rename, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { registerProvider } from "@flue/runtime";
import {
  type OAuthCredentials,
  refreshOpenAICodexToken,
} from "@earendil-works/pi-ai/oauth";

const PROVIDER_ID = "openai-codex";
// pi-ai's getOAuthApiKey only refreshes once already-expired; we add our own skew
// so a turn never starts on a token that is about to die.
const REFRESH_SKEW_MS = 5 * 60 * 1000;
// Keep the bearer fresh between HTTP requests (e.g. during a long multi-step turn).
const BACKGROUND_REFRESH_MS = 4 * 60 * 1000;

let creds: OAuthCredentials | null = null;
let inflight: Promise<void> | null = null;
let backgroundTimer: ReturnType<typeof setInterval> | null = null;

function codexHome(): string {
  const configured = process.env.CODEX_HOME?.trim();
  return configured ? configured : join(homedir(), ".codex");
}

function authPath(): string {
  return join(codexHome(), "auth.json");
}

function jwtExpiresMs(accessToken: string): number {
  const segments = accessToken.split(".");
  if (segments.length !== 3) {
    throw new Error("Codex access_token is not a JWT.");
  }
  const payload = JSON.parse(
    Buffer.from(segments[1], "base64url").toString("utf8"),
  ) as { exp?: unknown };
  if (typeof payload.exp !== "number") {
    throw new Error("Codex access_token JWT has no numeric exp claim.");
  }
  // JWT exp is seconds; OAuthCredentials.expires is epoch milliseconds (F8).
  return payload.exp * 1000;
}

async function seedFromAuthFile(): Promise<OAuthCredentials> {
  let raw: string;
  try {
    raw = await readFile(authPath(), "utf8");
  } catch {
    throw new Error(
      `Codex auth not found at ${authPath()}. Run \`codex login\` with a ChatGPT (Plus/Pro/Business) account first.`,
    );
  }
  const parsed = JSON.parse(raw) as {
    auth_mode?: unknown;
    tokens?: { access_token?: unknown; refresh_token?: unknown };
  };
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
  return {
    access: tokens.access_token,
    refresh: tokens.refresh_token,
    expires: jwtExpiresMs(tokens.access_token),
  };
}

async function persistRefreshed(next: OAuthCredentials): Promise<void> {
  // Keep ~/.codex/auth.json in sync so the codex CLI and the next boot use the
  // rotated tokens. Preserve every existing field; only update the token pair.
  let current: Record<string, unknown> = {};
  try {
    current = JSON.parse(await readFile(authPath(), "utf8")) as Record<
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
  tokens.access_token = next.access;
  tokens.refresh_token = next.refresh;
  const merged = {
    ...current,
    auth_mode: "chatgpt",
    tokens,
    last_refresh: new Date().toISOString(),
  };
  // Atomic replace so a concurrent codex CLI never reads a half-written file.
  const tmp = `${authPath()}.nav-${process.pid}.tmp`;
  await writeFile(tmp, `${JSON.stringify(merged, null, 2)}\n`, { mode: 0o600 });
  await rename(tmp, authPath());
}

async function refreshIfNeeded(): Promise<void> {
  if (!creds) {
    creds = await seedFromAuthFile();
  }
  if (Date.now() < creds.expires - REFRESH_SKEW_MS) {
    return;
  }
  const next = await refreshOpenAICodexToken(creds.refresh);
  creds = next;
  try {
    await persistRefreshed(next);
  } catch {
    // Best-effort persistence; the in-memory bearer is enough for this process.
  }
}

/**
 * Ensure the `openai-codex` provider is registered with a fresh ChatGPT/Codex
 * subscription bearer. Single-flight so concurrent requests share one refresh.
 *
 * `openai-codex` is a pi-ai catalog provider, so registering with only `apiKey`
 * hydrates the full catalog model — gpt-5.5 with `reasoning: true`,
 * `thinkingLevelMap`, `api: openai-codex-responses`, and the chatgpt.com baseUrl
 * (F2/F3). The provider extracts `chatgpt_account_id` from the JWT itself, so no
 * extra headers are needed (F5). Re-registering replaces the stored apiKey,
 * which Flue reads at model-call time — this is the bearer-rotation mechanism (F10).
 */
export async function ensureCodexProvider(): Promise<void> {
  if (!inflight) {
    inflight = (async () => {
      try {
        await refreshIfNeeded();
        if (!creds) {
          throw new Error("Codex credentials unavailable.");
        }
        registerProvider(PROVIDER_ID, { apiKey: creds.access });
      } finally {
        inflight = null;
      }
    })();
  }
  return inflight;
}

/**
 * Background timer keeping the bearer fresh between HTTP requests. Idempotent;
 * call once at boot.
 */
export function startCodexProviderRefresh(): void {
  if (backgroundTimer) {
    return;
  }
  backgroundTimer = setInterval(() => {
    void ensureCodexProvider().catch(() => {
      // Errors are surfaced per-request by the middleware; ignore here.
    });
  }, BACKGROUND_REFRESH_MS);
  backgroundTimer.unref?.();
}
```

### Step 4 — Rewrite the agent: `packages/flue/.flue/agents/nav.ts`

```ts
import {
  type AgentRouteHandler,
  defineAgent,
  type ThinkingLevel,
} from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { getWorkspaceRoot } from "../shared/codex.js";

const VALID_THINKING_LEVELS: ThinkingLevel[] = [
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
];

function resolveThinkingLevel(): ThinkingLevel {
  const configured = process.env.NAV_AGENT_THINKING_LEVEL?.trim() as
    | ThinkingLevel
    | undefined;
  return configured && VALID_THINKING_LEVELS.includes(configured)
    ? configured
    : "xhigh";
}

export const description =
  "Nav is a coding chat agent for the Nav codebase, running on the user's ChatGPT/Codex (gpt-5.5) subscription.";

// Exporting `route` exposes the agent at POST/GET /api/agents/nav/:id.
export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

export default defineAgent(() => {
  const repoRoot = getWorkspaceRoot();

  return {
    model: "openai-codex/gpt-5.5",
    thinkingLevel: resolveThinkingLevel(),
    sandbox: local({ cwd: repoRoot }),
    instructions: [
      `You are Nav, a coding assistant for the Nav monorepo at ${repoRoot}.`,
      "Use your file and command tools to read the codebase, investigate, debug, review, and explain.",
      "Be concise. Reference code as path:line so the user can click it.",
      "Do NOT create, modify, or delete files, and do NOT run mutating commands (writes, installs, git commit/push), unless the user explicitly asks you to make changes.",
    ].join(" "),
  };
});
```

> `thinkingLevel` defaults to **`'xhigh'`** (user decision, 2026-06-27) — maximum reasoning,
> and explicitly present in gpt-5.5's `thinkingLevelMap` (lowest-risk effort value). Override
> with `NAV_AGENT_THINKING_LEVEL=high` for snappier interactive turns, or `medium` for the
> model default — no code change needed.

### Step 5 — Export `getWorkspaceRoot` from `packages/flue/.flue/shared/codex.ts`

Single change — add `export` to the existing helper (everything else in the file stays;
`getCodexAuthStatus` is still used by the `/auth/codex/status` route, and
`runCodexTaskTool` remains available for the optional hybrid in §8):

```diff
-function getWorkspaceRoot(): string {
+export function getWorkspaceRoot(): string {
```

### Step 6 — Rewrite `packages/flue/.flue/app.ts`

```ts
import { flue } from "@flue/runtime/routing";
import { Hono, type MiddlewareHandler } from "hono";
import { cors } from "hono/cors";
import {
  ensureCodexProvider,
  startCodexProviderRefresh,
} from "./shared/codex-provider.js";
import { getCodexAuthStatus } from "./shared/codex.js";

const app = new Hono();
const streamHeaders = [
  "Stream-Next-Offset",
  "Stream-Up-To-Date",
  "Stream-Closed",
  "Stream-Cursor",
  "ETag",
];

const getAllowedDesktopOrigins = () =>
  new Set(
    (process.env.NAV_DESKTOP_ORIGIN ?? "")
      .split(",")
      .map((origin) => origin.trim())
      .filter(Boolean),
  );

const requireDesktopAuth: MiddlewareHandler = async (c, next) => {
  if (c.req.method === "OPTIONS") {
    return next();
  }

  const expectedToken = process.env.NAV_DESKTOP_TOKEN;

  if (!expectedToken) {
    return c.json({ error: "desktop_auth_not_configured" }, 503);
  }

  const token = c.req.header("authorization")?.match(/^Bearer\s+(.+)$/i)?.[1];

  if (token !== expectedToken) {
    return c.notFound();
  }

  return next();
};

// Refresh + (re)register the Codex subscription bearer before every agent call.
const requireCodexProvider: MiddlewareHandler = async (c, next) => {
  if (c.req.method === "OPTIONS") {
    return next();
  }

  try {
    await ensureCodexProvider();
  } catch (error) {
    return c.json(
      {
        error: "codex_auth_unavailable",
        message:
          error instanceof Error
            ? error.message
            : "Codex subscription auth is unavailable.",
      },
      503,
    );
  }

  return next();
};

// Warm the provider at boot (non-fatal) and keep it fresh between requests.
void ensureCodexProvider().catch((error: unknown) => {
  console.error(
    "[nav] Codex provider not ready at boot:",
    error instanceof Error ? error.message : error,
  );
});
startCodexProviderRefresh();

app.get("/health", (c) =>
  c.json({
    ok: true,
    service: "@nav/flue",
  }),
);

app.get("/auth/codex/status", async (c) => {
  const auth = await getCodexAuthStatus();

  return c.json({
    ok: auth.status === "ready",
    auth,
  });
});

app.use(
  "/api/*",
  cors({
    allowHeaders: ["Authorization", "Content-Type"],
    allowMethods: ["GET", "HEAD", "POST", "OPTIONS"],
    exposeHeaders: streamHeaders,
    maxAge: 600,
    origin: (origin) => {
      const allowedOrigins = getAllowedDesktopOrigins();

      return allowedOrigins.has(origin) ? origin : null;
    },
  }),
);
app.use("/api/agents/*", requireDesktopAuth);
app.use("/api/agents/*", requireCodexProvider);
app.use("/api/workflows/*", requireDesktopAuth);
app.use("/api/runs/*", requireDesktopAuth);
app.route("/api", flue());

export default app;
```

### Step 7 — Delete the workflow

```bash
rm packages/flue/.flue/workflows/codex-plan.ts
rmdir packages/flue/.flue/workflows 2>/dev/null || true
```

Nothing else imports `codex-plan`; the desktop call to it is removed in Step 8.

### Step 8 — Rewrite the desktop chat: `packages/desktop/src/main.tsx`

Replace the whole file with the version below. Changes vs. current: import
`useFlueAgent`/`UIMessage` from `@flue/react`; drop the manual `messages` state,
`buildCodexPrompt`, `createTextMessage`, `CodexTaskResult`, `ChatMessage`, and the
`client.workflows.run` call; `NavChat` now uses `useFlueAgent({ name:'nav', id })`.
Everything else (connection bootstrap, empty states, layout) is unchanged.

```tsx
import { FlueProvider, type UIMessage, useFlueAgent } from "@flue/react";
import { createFlueClient } from "@flue/sdk";
import {
  CircleAlertIcon,
  LoaderCircleIcon,
  MessageSquareTextIcon,
} from "lucide-react";
import { StrictMode, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";

import {
  Conversation,
  ConversationContent,
  ConversationScrollButton,
} from "@/components/ai-elements/conversation";
import {
  Message,
  MessageContent,
  MessageResponse,
} from "@/components/ai-elements/message";
import {
  PromptInput,
  PromptInputBody,
  PromptInputFooter,
  PromptInputSubmit,
  PromptInputTextarea,
  PromptInputTools,
} from "@/components/ai-elements/prompt-input";
import { AppSidebar } from "@/components/app-sidebar";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { TooltipProvider } from "@/components/ui/tooltip";
import type { FlueConnection, FlueServerStatus } from "@/lib/flue-connection";

import "./styles.css";

const getMessageText = (message: UIMessage) =>
  message.parts
    .map((part) =>
      part.type === "text" || part.type === "reasoning" ? part.text : "",
    )
    .join("");

const formatUuidBytes = (bytes: Uint8Array) =>
  [...bytes]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("")
    .replace(/^(.{8})(.{4})(.{4})(.{4})(.{12})$/, "$1-$2-$3-$4-$5");

const createUuidV7 = () => {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);

  const timestamp = BigInt(Date.now());
  bytes[0] = Number((timestamp >> 40n) & 0xffn);
  bytes[1] = Number((timestamp >> 32n) & 0xffn);
  bytes[2] = Number((timestamp >> 24n) & 0xffn);
  bytes[3] = Number((timestamp >> 16n) & 0xffn);
  bytes[4] = Number((timestamp >> 8n) & 0xffn);
  bytes[5] = Number(timestamp & 0xffn);
  bytes[6] = (bytes[6] & 0x0f) | 0x70;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;

  return formatUuidBytes(bytes);
};

function EmptyConversation() {
  return (
    <Empty className="min-h-0 border-0 px-6 py-10">
      <EmptyHeader>
        <EmptyMedia className="size-10 rounded-xl" variant="icon">
          <MessageSquareTextIcon aria-hidden="true" className="size-5" />
        </EmptyMedia>
        <EmptyTitle>Message Nav</EmptyTitle>
        <EmptyDescription>
          Start a conversation with the local Nav agent.
        </EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

function ConnectionEmpty({
  message,
  state,
}: {
  message: string;
  state: "failed" | "starting";
}) {
  const Icon = state === "failed" ? CircleAlertIcon : LoaderCircleIcon;

  return (
    <Empty className="min-h-0 border-0 px-6 py-10">
      <EmptyHeader>
        <EmptyMedia className="size-10 rounded-xl" variant="icon">
          <Icon
            aria-hidden="true"
            className={state === "starting" ? "size-5 animate-spin" : "size-5"}
          />
        </EmptyMedia>
        <EmptyTitle>
          {state === "failed" ? "Nav is unavailable" : "Starting Nav"}
        </EmptyTitle>
        <EmptyDescription>{message}</EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

function LiveConversation({ messages }: { messages: UIMessage[] }) {
  if (messages.length === 0) {
    return <EmptyConversation />;
  }

  return (
    <Conversation className="min-h-0">
      <ConversationContent className="mx-auto w-full max-w-3xl px-6 pt-14 pb-8">
        {messages.map((message) => (
          <Message
            from={message.role === "assistant" ? "assistant" : "user"}
            key={message.id}
          >
            <MessageContent>
              <MessageResponse>{getMessageText(message)}</MessageResponse>
            </MessageContent>
          </Message>
        ))}
      </ConversationContent>
      <ConversationScrollButton aria-label="Scroll to bottom" />
    </Conversation>
  );
}

function PromptComposer({
  disabled,
  onSubmit,
  status,
}: {
  disabled?: boolean;
  onSubmit: (message: string) => Promise<void>;
  status?: "error" | "submitted" | "streaming";
}) {
  return (
    <div className="shrink-0 bg-background/95 px-4 py-3 backdrop-blur">
      <PromptInput
        aria-label="Chat prompt"
        className="mx-auto max-w-3xl"
        onSubmit={async (message) => {
          const text = message.text.trim();

          if (!text || disabled) {
            return;
          }

          await onSubmit(text);
        }}
      >
        <PromptInputBody>
          <PromptInputTextarea disabled={disabled} placeholder="Message Nav" />
        </PromptInputBody>
        <PromptInputFooter>
          <PromptInputTools />
          <PromptInputSubmit disabled={disabled} status={status} />
        </PromptInputFooter>
      </PromptInput>
    </div>
  );
}

function NavChat({ serverStatus }: { serverStatus: FlueServerStatus | null }) {
  const [conversationId] = useState(() => createUuidV7());
  const { messages, status, error, historyReady, sendMessage } = useFlueAgent({
    id: conversationId,
    name: "nav",
  });

  const serverReady = serverStatus?.state === "ready";
  const busy =
    status === "connecting" || status === "submitted" || status === "streaming";
  const disabled = !serverReady || !historyReady || busy;

  const composerStatus =
    status === "error"
      ? "error"
      : status === "streaming"
        ? "streaming"
        : busy
          ? "submitted"
          : undefined;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {error && (
        <div className="mx-auto mt-4 w-full max-w-3xl rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-destructive text-sm">
          {error.message}
        </div>
      )}
      <LiveConversation messages={messages} />
      <PromptComposer
        disabled={disabled}
        onSubmit={async (text) => {
          await sendMessage(text);
        }}
        status={composerStatus}
      />
    </div>
  );
}

function ConnectedApp({
  connection,
  serverStatus,
}: {
  connection: FlueConnection;
  serverStatus: FlueServerStatus | null;
}) {
  const client = useMemo(
    () =>
      createFlueClient({
        baseUrl: connection.baseUrl,
        fetch: window.fetch.bind(window),
        token: connection.token,
      }),
    [connection.baseUrl, connection.token],
  );

  return (
    <FlueProvider client={client}>
      <NavChat serverStatus={serverStatus} />
    </FlueProvider>
  );
}

function AppContent() {
  const [connection, setConnection] = useState<FlueConnection | null>(null);
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [serverStatus, setServerStatus] = useState<FlueServerStatus | null>(
    null,
  );

  useEffect(() => {
    const unsubscribe = window.navDesktop.onFlueStatus(setServerStatus);

    window.navDesktop
      .getFlueConnection()
      .then((nextConnection) => {
        setConnection(nextConnection);
        setServerStatus(nextConnection.status);
      })
      .catch((error: unknown) => {
        setConnectionError(
          error instanceof Error ? error.message : "Unable to connect to Nav.",
        );
      });

    return unsubscribe;
  }, []);

  if (connectionError) {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <ConnectionEmpty message={connectionError} state="failed" />
      </div>
    );
  }

  if (!connection) {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <ConnectionEmpty
          message={
            serverStatus?.message ?? "Waiting for the local Flue server."
          }
          state={serverStatus?.state === "failed" ? "failed" : "starting"}
        />
      </div>
    );
  }

  return <ConnectedApp connection={connection} serverStatus={serverStatus} />;
}

function App() {
  return (
    <TooltipProvider>
      <SidebarProvider>
        <AppSidebar />
        <div className="fixed inset-x-0 top-0 z-40 h-10 [-webkit-app-region:drag]" />
        <SidebarTrigger className="fixed top-1 left-[76px] z-50 [-webkit-app-region:no-drag] [&_svg]:!size-[18px]" />
        <SidebarInset className="min-h-svh overflow-hidden pt-10">
          <AppContent />
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  );
}

const root = document.createElement("div");
root.id = "root";
document.body.replaceChildren(root);

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
```

> If `Message`'s `from` prop type rejects the ternary, it already accepts
> `"user" | "assistant"`; the ternary maps any non-assistant role (incl. `system`) to
> `"user"`. Leave it as written.

### Step 9 — Ignore the sqlite data dir

Append to `.gitignore` (near the existing `packages/flue/dist/` block):

```gitignore
packages/flue/data/
```

---

## 5. Verification

Run from the repo root unless noted.

1. **Install + typecheck**
   ```bash
   pnpm install
   pnpm --filter @nav/flue typecheck
   pnpm --filter @nav/desktop exec tsc -p tsconfig.main.json --noEmit   # main process
   pnpm --filter @nav/desktop exec tsc --noEmit                         # renderer (if a renderer tsconfig exists)
   ```
   Expect no errors. (Adjust the desktop typecheck command to the package's actual script
   if one is defined.)

2. **Flue server boots + the agent answers on the subscription** (standalone, no Electron):
   ```bash
   cd packages/flue
   NAV_DESKTOP_TOKEN=devtoken NAV_DESKTOP_ORIGIN=null pnpm dev --port 3599 &
   # wait for health
   curl -sf http://127.0.0.1:3599/health
   curl -s http://127.0.0.1:3599/auth/codex/status | jq .   # expect ok:true, mode "chatgpt"
   # open the agent stream (GET) and send a prompt (POST) for instance id "smoke-1"
   curl -s -H "Authorization: Bearer devtoken" \
        -H "Content-Type: application/json" \
        -X POST http://127.0.0.1:3599/api/agents/nav/smoke-1 \
        -d '{"message":"In one sentence, what is in packages/flue/.flue/agents/nav.ts?"}'
   ```
   - A 200 with streamed/located content that references the file = the subscription brain
     works. (Exact POST body shape: the desktop uses `useFlueAgent().sendMessage(text)`;
     if the raw curl body differs, rely on the UI test in step 4 as the source of truth.)
   - A **503 `codex_auth_unavailable`** = `~/.codex/auth.json` missing/not `chatgpt`; run
     `codex login`.
   - Watch the server log for a real model call to `chatgpt.com/backend-api` (not
     `api.openai.com`), confirming subscription billing rather than API-key billing.

3. **Bearer / reasoning sanity**
   - Confirm no `401`/`invalid reasoning_effort` errors in the server log on the first turn
     (validates F4 — `thinkingLevel:'xhigh'` accepted) and that reasoning streams.
   - Leave a conversation idle > the background interval and send another message; it
     should still answer (validates rotation path doesn't break).

4. **Desktop UI (test like a human — per project practice):**
   ```bash
   pnpm --filter @nav/desktop dev   # or the repo's electron dev command
   ```
   - Type a question in the chat, confirm a streamed assistant reply renders.
   - Ask it to read a specific file (e.g. "summarize packages/flue/.flue/app.ts") and
     confirm it uses its sandbox tools and answers.
   - Ask it to make a trivial change *without* permission — it should decline per
     instructions; then explicitly authorize a change and confirm it can edit.
   - Restart the app, send a new message — a fresh conversation id each launch is expected
     (history persistence per-id is in the sqlite store; cross-launch resume is out of
     scope here, see §8).

5. **Cleanup:** `kill` the standalone flue dev server.

---

## 6. Security / safety notes

- `local()` is **not** an isolation boundary (F17). This is intentional: Nav is a trusted
  local dev tool operating on the user's own checkout. The instructions keep it read-mostly
  unless the user explicitly authorizes changes.
- `persistRefreshed` writes `~/.codex/auth.json` **atomically** (temp + `rename`, mode
  `0600`) and preserves all existing fields, so it stays compatible with the `codex` CLI.
  It only fires when a token is within 5 min of expiry (rare; current TTL ≈ 42h).
- The desktop token + CORS origin gates already protect `/api/agents/*`; the new
  `requireCodexProvider` middleware runs only after `requireDesktopAuth`.

---

## 7. Risks & rollback

| Risk | Mitigation |
|------|-----------|
| `pnpm add` pulls a pi-ai newer than 0.79.10 → duplicate instance | Pin exactly `0.79.10`; verify a single copy with `pnpm why @earendil-works/pi-ai`. |
| `node:sqlite` unavailable in the runtime | Verified flagless on Node 24.16 (F13). Fallback: change `db.ts` to `sqlite()` (in-memory) or delete `db.ts` (defaults to `:memory:`). |
| `xhigh` turns feel slow/expensive for simple chat | Set `NAV_AGENT_THINKING_LEVEL=high` (or `medium`) — single env change, no code edit. Default `xhigh` is the explicitly-mapped, lowest-risk effort. |
| Raw curl POST body shape differs from the hook's | The UI (`useFlueAgent`) is the contract; trust step 4 over step 2's curl. |
| Token refresh fails (revoked refresh token) | Middleware returns 503 with a clear message; user re-runs `codex login`. |

**Rollback:** revert the 8 changed files and restore `workflows/codex-plan.ts` from git.
No data migration is involved (the sqlite file is disposable).

---

## 8. Optional / out of scope (do not block on these)

- **Hybrid `run_codex_task` tool.** `shared/codex.ts` still exports `runCodexTaskTool`
  (`@openai/codex-sdk`). To let Nav delegate a full autonomous Codex run, add it to the
  agent: `import { runCodexTaskTool } from "../shared/codex.js";` and
  `tools: [runCodexTaskTool]` in `nav.ts`. Redundant with the local sandbox for ordinary
  chat, so it's off by default.
- **Multi-conversation history / sidebar.** Persisting and switching conversation ids
  (e.g. via `localStorage` + the sidebar) so a relaunch resumes a prior chat. The store
  already keys history by agent instance id; only the UI plumbing is missing.
- **Image input.** `useFlueAgent.sendMessage` accepts `{ images }`; gpt-5.5 catalog input
  includes `image`. Wire the composer's attachments to it later.
```
