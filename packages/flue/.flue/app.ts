import { flue } from "@flue/runtime/routing";
import { Hono } from "hono";
import { getCodexAuthStatus } from "./shared/codex.js";

const app = new Hono();

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

// TODO: Require authenticated desktop requests before wiring this into the app.
app.route("/api", flue());

export default app;
