import { flue } from "@flue/runtime/routing";
import { Hono, type MiddlewareHandler } from "hono";
import { cors } from "hono/cors";
import { getCodexAuthStatus } from "./shared/codex.js";
import {
  ensureCodexProvider,
  startCodexProviderRefresh,
} from "./shared/codex-provider.js";
import { ensureDeepseekProvider } from "./shared/deepseek-provider.js";
import {
  ensureNavProjectsReady,
  handleCreateNavProject,
  handleDeleteNavProject,
  handleListNavProjects,
  handleUpdateNavProject,
} from "./shared/nav-projects.js";
import {
  ensureNavSessionsReady,
  handleCreateNavSession,
  handleDeleteNavSession,
  handleListNavSessions,
  handleUpdateNavSession,
} from "./shared/nav-sessions.js";
import { pruneAgentWorktrees } from "./shared/worktrees.js";
import { ensureZaiProvider } from "./shared/zai-provider.js";

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

void ensureCodexProvider().catch((error: unknown) => {
  console.error(
    "[nav] Codex provider not ready at boot:",
    error instanceof Error ? error.message : error,
  );
});
startCodexProviderRefresh();
ensureDeepseekProvider();
ensureZaiProvider();
void ensureNavSessionsReady()
  .then(() => ensureNavProjectsReady())
  .catch((error: unknown) => {
    console.error(
      "[nav] Session/project registry not ready at boot:",
      error instanceof Error ? error.message : error,
    );
  });
try {
  pruneAgentWorktrees();
} catch (error) {
  console.warn(
    "[nav] Failed to prune stale agent worktrees:",
    error instanceof Error ? error.message : error,
  );
}

app.get("/health", (c) =>
  c.json({
    ok: true,
    service: "@nav/flue",
  }),
);

app.get("/auth/codex/status", async (c) => {
  const auth = await getCodexAuthStatus();

  return c.json({
    // The Nav agent runs on the ChatGPT subscription bearer specifically, so a
    // ready API-key or access-token credential is not usable here. Report `ok`
    // only when it matches what the agent path actually requires, otherwise the
    // status contradicts the 503 every chat request would return.
    ok: auth.status === "ready" && auth.mode === "chatgpt",
    auth,
  });
});

app.use(
  "/api/*",
  cors({
    allowHeaders: ["Authorization", "Content-Type"],
    allowMethods: ["GET", "HEAD", "POST", "PATCH", "DELETE", "OPTIONS"],
    exposeHeaders: streamHeaders,
    maxAge: 600,
    origin: (origin) => {
      const allowedOrigins = getAllowedDesktopOrigins();

      return allowedOrigins.has(origin) ? origin : null;
    },
  }),
);
app.use("/api/*", requireDesktopAuth);
app.get("/api/projects", handleListNavProjects);
app.post("/api/projects", handleCreateNavProject);
app.patch("/api/projects/:id", handleUpdateNavProject);
app.delete("/api/projects/:id", handleDeleteNavProject);
app.get("/api/sessions", handleListNavSessions);
app.post("/api/sessions", handleCreateNavSession);
app.patch("/api/sessions/:id", handleUpdateNavSession);
app.delete("/api/sessions/:id", handleDeleteNavSession);
app.use("/api/agents/nav/*", requireCodexProvider);
app.route("/api", flue());

export default app;
