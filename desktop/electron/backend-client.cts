import { randomUUID } from "node:crypto";
import http from "node:http";
import https from "node:https";
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
  const eventsUrl = new URL(`/sessions/${sessionId}/events`, backendUrl);
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
      const parsed = parseSseBuffer(buffer);
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

function parseSseBuffer(buffer: string): {
  events: SessionEvent[];
  remainder: string;
} {
  const events: SessionEvent[] = [];
  const frames = buffer.split(/\n\n/);
  const remainder = frames.pop() ?? "";

  for (const frame of frames) {
    const event = parseSseFrame(frame);
    if (event) {
      events.push(event);
    }
  }

  return { events, remainder };
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
  const rpcUrl = new URL("/rpc", backendUrl);
  const transport = rpcUrl.protocol === "https:" ? https : http;
  const body = JSON.stringify({
    jsonrpc: "2.0",
    id: randomUUID(),
    method,
    ...(params ? { params } : {}),
  });

  return new Promise((resolve, reject) => {
    const request = transport.request(
      rpcUrl,
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "content-length": Buffer.byteLength(body),
        },
      },
      (response) => {
        let payload = "";
        response.setEncoding("utf8");
        response.on("data", (chunk: string) => {
          payload += chunk;
        });
        response.on("end", () => {
          let parsed: RpcResponse;
          try {
            parsed = JSON.parse(payload) as RpcResponse;
          } catch (error) {
            reject(
              new Error(
                `RPC ${method} returned invalid JSON: ${(error as Error).message}`,
              ),
            );
            return;
          }
          if (parsed.error) {
            reject(new Error(`RPC ${method} failed: ${parsed.error.message}`));
            return;
          }
          resolve(parsed);
        });
      },
    );

    request.on("error", reject);
    request.write(body);
    request.end();
  });
}

function parseSseFrame(frame: string): SessionEvent | null {
  const dataLine = frame.split(/\n/).find((line) => line.startsWith("data: "));
  if (!dataLine) {
    return null;
  }

  return JSON.parse(dataLine.slice("data: ".length)) as SessionEvent;
}
