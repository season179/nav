import { type Context, Hono } from "hono";
import type { ContentfulStatusCode } from "hono/utils/http-status";
import type { BackendServices } from "./services.js";
import type { SessionMode } from "./worktrees.js";

export function createControlPlane(services: BackendServices): Hono {
  const app = new Hono();

  app.post("/sessions", async (c) =>
    handle(c, async () => {
      const body = await readBody(c.req.raw);
      const session = await services.catalog.create({
        cwd: optionalString(body.cwd),
        mode: optionalMode(body.mode),
      });
      return { sessionId: session.sessionId };
    }),
  );

  app.get("/sessions", async (c) =>
    handle(c, async () => ({ sessions: await services.catalog.list() })),
  );

  app.get("/sessions/latest", async (c) =>
    handle(c, async () => ({
      sessionId: await services.catalog.latestByCwd(c.req.query("cwd")),
    })),
  );

  app.post("/sessions/:sessionId/resume", async (c) =>
    handle(c, async () => {
      const session = await requireSession(services, c.req.param("sessionId"));
      await services.catalog.resume(session.sessionId);
      return { sessionId: session.sessionId };
    }),
  );

  app.delete("/sessions/:sessionId", async (c) =>
    handle(c, async () => {
      const sessionId = c.req.param("sessionId");
      await requireSession(services, sessionId);
      await services.catalog.delete(sessionId);
      await services.stacks.deleteSession(sessionId);
      return { deleted: true };
    }),
  );

  app.post("/sessions/:sessionId/stop", async (c) =>
    handle(c, async () => {
      await requireSession(services, c.req.param("sessionId"));
      // Flue beta.5 documents in-process AbortSignal support, but no HTTP
      // endpoint that guarantees durable agent submission cancellation.
      return { stopped: false };
    }),
  );

  app.get("/models", (c) => c.json({ models: services.models.list() }));
  app.get("/model", (c) => c.json(services.models.defaultModelInfo()));

  app.get("/sessions/:sessionId/model", async (c) =>
    handle(c, async () => {
      const session = await requireSession(services, c.req.param("sessionId"));
      return services.models.modelInfo(session);
    }),
  );

  app.post("/sessions/:sessionId/model", async (c) =>
    handle(c, async () => {
      const sessionId = c.req.param("sessionId");
      await requireSession(services, sessionId);
      const body = await readBody(c.req.raw);
      const provider = requiredString(body.provider, "provider");
      const model = requiredString(body.model, "model");
      const selection = services.models.resolveSelection({
        provider,
        model,
        thinkingLevel: optionalString(body.thinkingLevel),
      });
      const session = await services.catalog.updateModel(sessionId, selection);

      if (!session) {
        throw new HttpError(404, "session not found");
      }

      return { modelInfo: services.models.modelInfo(session) };
    }),
  );

  app.post("/sessions/:sessionId/thinking", async (c) =>
    handle(c, async () => {
      const sessionId = c.req.param("sessionId");
      const session = await requireSession(services, sessionId);
      const body = await readBody(c.req.raw);
      const selection = services.models.switchThinking(
        session,
        requiredString(body.thinkingLevel, "thinkingLevel"),
      );
      const updated = await services.catalog.updateThinking(
        sessionId,
        selection.thinkingLevel,
      );

      if (!updated) {
        throw new HttpError(404, "session not found");
      }

      return { modelInfo: services.models.modelInfo(updated) };
    }),
  );

  app.get("/sessions/:sessionId/stacks", async (c) =>
    handle(c, async () => {
      const sessionId = c.req.param("sessionId");
      await requireSession(services, sessionId);
      return services.stacks.list(sessionId);
    }),
  );

  app.get("/sessions/:sessionId/stacks/availability", async (c) =>
    handle(c, async () => {
      await requireSession(services, c.req.param("sessionId"));
      return { available: true };
    }),
  );

  return app;
}

class HttpError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function handle<T>(c: Context, run: () => Promise<T>): Promise<Response> {
  try {
    return c.json(await run());
  } catch (error) {
    const status = error instanceof HttpError ? error.status : 500;
    const message = error instanceof Error ? error.message : "unknown error";
    return c.json({ error: { message } }, status as ContentfulStatusCode);
  }
}

async function requireSession(services: BackendServices, sessionId: string) {
  const session = await services.catalog.get(sessionId);
  if (!session) {
    throw new HttpError(404, "session not found");
  }

  return session;
}

async function readBody(request: Request): Promise<Record<string, unknown>> {
  if (!request.headers.get("content-type")?.includes("application/json")) {
    return {};
  }

  const body = (await request.json()) as unknown;
  return body && typeof body === "object"
    ? (body as Record<string, unknown>)
    : {};
}

function optionalString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function requiredString(value: unknown, name: string): string {
  const parsed = optionalString(value);
  if (!parsed) {
    throw new HttpError(400, `${name} is required`);
  }

  return parsed;
}

function optionalMode(value: unknown): SessionMode | null {
  if (value === "local" || value === "worktree") {
    return value;
  }

  return null;
}
