import { randomUUID } from "node:crypto";
import http from "node:http";
import https from "node:https";
import { parseFlueSseBuffer } from "./flue-session-events.cjs";
import type { RpcResponse, SessionEvent } from "./types.cjs";

export type Subscription = { close(): void };

type SubscribeOptions = {
  backendUrl: string;
  sessionId: string;
  signal?: AbortSignal;
  onEvent: (event: SessionEvent) => void;
  onError: (error: Error) => void;
  onOpen?: (info: { statusCode?: number }) => void;
};

export function subscribeToSessionEvents({
  backendUrl,
  sessionId,
  signal,
  onEvent,
  onError,
  onOpen,
}: SubscribeOptions): Subscription {
  const eventsUrl = new URL(
    `/agents/nav/${encodeURIComponent(sessionId)}`,
    backendUrl,
  );
  eventsUrl.searchParams.set("live", "sse");
  eventsUrl.searchParams.set("offset", "-1");
  const transport = eventsUrl.protocol === "https:" ? https : http;
  let buffer = "";
  let closed = false;

  const request = transport.get(eventsUrl, (response) => {
    onOpen?.({ statusCode: response.statusCode });
    if (response.statusCode !== 200) {
      response.resume();
      onError(new Error(`SSE request failed with HTTP ${response.statusCode}`));
      return;
    }

    response.setEncoding("utf8");
    response.on("data", (chunk: string) => {
      buffer += chunk;
      const parsed = parseFlueSseBuffer(buffer, { sessionId });
      buffer = parsed.remainder;
      for (const event of parsed.events) {
        onEvent(event);
      }
    });
    response.on("error", (error) => {
      if (!closed && !signal?.aborted) {
        onError(error);
      }
    });
  });

  request.on("error", (error) => {
    if (!closed && !signal?.aborted) {
      onError(error);
    }
  });

  signal?.addEventListener(
    "abort",
    () => {
      closed = true;
      request.destroy();
    },
    { once: true },
  );

  return {
    close() {
      closed = true;
      request.destroy();
    },
  };
}

export function sendRpc({
  backendUrl,
  method,
  params,
}: {
  backendUrl: string;
  method: string;
  params?: unknown;
}): Promise<RpcResponse> {
  const id = randomUUID();

  return sendBackendMethod({ backendUrl, method, params }).then((result) => ({
    jsonrpc: "2.0",
    id,
    result,
  }));
}

async function sendBackendMethod({
  backendUrl,
  method,
  params,
}: {
  backendUrl: string;
  method: string;
  params?: unknown;
}): Promise<unknown> {
  const values = paramsObject(params);

  switch (method) {
    case "session.create":
      return requestJson({
        url: new URL("/nav/sessions", backendUrl),
        method: "POST",
        body: values,
      });
    case "session.list":
      return requestJson({
        url: new URL("/nav/sessions", backendUrl),
        method: "GET",
      });
    case "session.latest": {
      const url = new URL("/nav/sessions/latest", backendUrl);
      const cwd = optionalString(values.cwd);
      if (cwd) {
        url.searchParams.set("cwd", cwd);
      }
      return requestJson({ url, method: "GET" });
    }
    case "session.resume": {
      const sessionId = requiredString(values.sessionId, "sessionId");
      return requestJson({
        url: new URL(
          `/nav/sessions/${encodeURIComponent(sessionId)}/resume`,
          backendUrl,
        ),
        method: "POST",
      });
    }
    case "session.delete": {
      const sessionId = requiredString(values.sessionId, "sessionId");
      return requestJson({
        url: new URL(
          `/nav/sessions/${encodeURIComponent(sessionId)}`,
          backendUrl,
        ),
        method: "DELETE",
      });
    }
    case "session.sendMessage": {
      const sessionId = requiredString(values.sessionId, "sessionId");
      const text = requiredString(values.text, "text");
      return requestJson({
        url: new URL(
          `/agents/nav/${encodeURIComponent(sessionId)}`,
          backendUrl,
        ),
        method: "POST",
        body: { message: text },
      });
    }
    case "session.stop": {
      const sessionId = requiredString(values.sessionId, "sessionId");
      return requestJson({
        url: new URL(
          `/nav/sessions/${encodeURIComponent(sessionId)}/stop`,
          backendUrl,
        ),
        method: "POST",
      });
    }
    case "session.modelInfo": {
      const sessionId = optionalString(values.sessionId);
      return requestJson({
        url: new URL(
          sessionId
            ? `/nav/sessions/${encodeURIComponent(sessionId)}/model`
            : "/nav/model",
          backendUrl,
        ),
        method: "GET",
      });
    }
    case "session.models":
      return requestJson({
        url: new URL("/nav/models", backendUrl),
        method: "GET",
      });
    case "session.switchModel": {
      const sessionId = requiredString(values.sessionId, "sessionId");
      return requestJson({
        url: new URL(
          `/nav/sessions/${encodeURIComponent(sessionId)}/model`,
          backendUrl,
        ),
        method: "POST",
        body: {
          provider: requiredString(values.provider, "provider"),
          model: requiredString(values.model, "model"),
          ...(optionalString(values.thinkingLevel)
            ? { thinkingLevel: values.thinkingLevel }
            : {}),
        },
      });
    }
    case "session.switchThinking": {
      const sessionId = requiredString(values.sessionId, "sessionId");
      return requestJson({
        url: new URL(
          `/nav/sessions/${encodeURIComponent(sessionId)}/thinking`,
          backendUrl,
        ),
        method: "POST",
        body: {
          thinkingLevel: requiredString(values.thinkingLevel, "thinkingLevel"),
        },
      });
    }
    case "session.stacks": {
      const sessionId = requiredString(values.sessionId, "sessionId");
      return requestJson({
        url: new URL(
          `/nav/sessions/${encodeURIComponent(sessionId)}/stacks`,
          backendUrl,
        ),
        method: "GET",
      });
    }
    case "session.stackAvailability": {
      const sessionId = requiredString(values.sessionId, "sessionId");
      return requestJson({
        url: new URL(
          `/nav/sessions/${encodeURIComponent(sessionId)}/stacks/availability`,
          backendUrl,
        ),
        method: "GET",
      });
    }
    default:
      throw new Error(`unsupported backend method: ${method}`);
  }
}

function requestJson({
  url,
  method,
  body,
}: {
  url: URL;
  method: "DELETE" | "GET" | "POST";
  body?: unknown;
}): Promise<unknown> {
  const transport = url.protocol === "https:" ? https : http;
  const payload = body ? JSON.stringify(body) : undefined;

  return new Promise((resolve, reject) => {
    const request = transport.request(
      url,
      {
        method,
        headers: payload
          ? {
              "content-type": "application/json",
              "content-length": Buffer.byteLength(payload),
            }
          : undefined,
      },
      (response) => {
        let payload = "";
        response.setEncoding("utf8");
        response.on("data", (chunk: string) => {
          payload += chunk;
        });
        response.on("end", () => {
          let parsed: unknown;
          try {
            parsed = payload ? JSON.parse(payload) : {};
          } catch (error) {
            reject(
              new Error(
                `${method} ${url.pathname} returned invalid JSON: ${
                  (error as Error).message
                }`,
              ),
            );
            return;
          }
          const backendError = readBackendError(parsed);
          if (!response.statusCode || response.statusCode >= 400) {
            reject(
              new Error(
                `${method} ${url.pathname} failed with HTTP ${
                  response.statusCode ?? "unknown"
                }${backendError ? `: ${backendError}` : ""}`,
              ),
            );
            return;
          }
          if (backendError) {
            reject(
              new Error(`${method} ${url.pathname} failed: ${backendError}`),
            );
            return;
          }
          resolve(parsed);
        });
      },
    );

    request.on("error", reject);
    if (payload) {
      request.write(payload);
    }
    request.end();
  });
}

function readBackendError(payload: unknown): string | null {
  if (!payload || typeof payload !== "object") {
    return null;
  }
  const error = (payload as { error?: unknown }).error;
  if (!error || typeof error !== "object") {
    return null;
  }
  const message = (error as { message?: unknown }).message;
  return typeof message === "string" ? message : null;
}

function paramsObject(params: unknown): Record<string, unknown> {
  return params && typeof params === "object"
    ? (params as Record<string, unknown>)
    : {};
}

function optionalString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function requiredString(value: unknown, name: string): string {
  const parsed = optionalString(value);
  if (!parsed) {
    throw new Error(`${name} is required`);
  }
  return parsed;
}
