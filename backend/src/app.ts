import { flue } from "@flue/runtime/routing";
import { Hono } from "hono";

const openApiDocument = {
  openapi: "3.1.0",
  info: {
    title: "nav local backend",
    version: "0.1.0",
  },
  paths: {
    "/health": {
      get: {
        summary: "Check backend readiness",
        responses: {
          "200": {
            description: "The backend is ready to accept local requests.",
          },
        },
      },
    },
    "/agents/nav/{id}": {
      get: {
        summary: "Read a nav agent event stream",
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "string" },
          },
        ],
        responses: {
          "200": { description: "Durable Streams response." },
        },
      },
      post: {
        summary: "Submit a prompt to a nav agent session",
        parameters: [
          {
            name: "id",
            in: "path",
            required: true,
            schema: { type: "string" },
          },
        ],
        requestBody: {
          required: true,
          content: {
            "application/json": {
              schema: {
                type: "object",
                required: ["message"],
                properties: {
                  message: { type: "string" },
                  images: {
                    type: "array",
                    items: { type: "object" },
                  },
                },
              },
            },
          },
        },
        responses: {
          "202": {
            description:
              "The prompt was accepted and stream coordinates returned.",
          },
        },
      },
    },
  },
} as const;

const app = new Hono();

app.get("/health", (c) =>
  c.json({
    ok: true,
    service: "nav-backend",
  }),
);

app.get("/openapi.json", (c) => c.json(openApiDocument));

app.route("/", flue());

export default app;
