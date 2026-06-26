import { flue } from "@flue/runtime/routing";
import { Hono } from "hono";

const app = new Hono();

app.get("/health", (c) =>
  c.json({
    ok: true,
    service: "@nav/flue",
  }),
);

// TODO: Require authenticated desktop requests before wiring this into the app.
app.route("/api", flue());

export default app;
