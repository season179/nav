import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";

import {
  type AgentRunner,
  MockAgentRunner,
  PiAgentRunner,
} from "./agent-runner.js";
import { ChatRequestError, readChatMessages } from "./chat-request.js";
import {
  DEFAULT_AGENT_HOST,
  DEFAULT_AGENT_PORT,
  SERVICE_NAME,
} from "./config.js";

export interface AgentServerOptions {
  runner?: AgentRunner;
}

const corsHeaders = (request: Request): HeadersInit => ({
  "Access-Control-Allow-Headers": "content-type",
  "Access-Control-Allow-Methods": "GET,POST,OPTIONS",
  "Access-Control-Allow-Origin": request.headers.get("origin") ?? "*",
});

const jsonResponse = (
  request: Request,
  value: unknown,
  init: ResponseInit = {},
) =>
  Response.json(value, {
    ...init,
    headers: {
      ...corsHeaders(request),
      ...Object.fromEntries(new Headers(init.headers).entries()),
    },
  });

export const handleAgentRequest = async (
  request: Request,
  runner: AgentRunner,
): Promise<Response> => {
  const url = new URL(request.url);

  if (request.method === "OPTIONS") {
    return new Response(null, { headers: corsHeaders(request), status: 204 });
  }

  if (request.method === "GET" && url.pathname === "/health") {
    return jsonResponse(request, { ok: true, service: SERVICE_NAME });
  }

  if (request.method === "POST" && url.pathname === "/api/chat") {
    try {
      const chatMessages = await readChatMessages(request);
      const response = await runner.createResponse(chatMessages, {
        signal: request.signal,
      });
      const headers = new Headers(response.headers);
      for (const [key, value] of Object.entries(corsHeaders(request))) {
        headers.set(key, value);
      }

      return new Response(response.body, {
        headers,
        status: response.status,
        statusText: response.statusText,
      });
    } catch (error) {
      if (error instanceof ChatRequestError) {
        return jsonResponse(
          request,
          { error: error.message },
          { status: error.status },
        );
      }
      throw error;
    }
  }

  return jsonResponse(request, { error: "Not found" }, { status: 404 });
};

const readIncomingBody = async (
  request: IncomingMessage,
): Promise<Buffer | undefined> => {
  if (request.method === "GET" || request.method === "HEAD") {
    return undefined;
  }

  const chunks: Buffer[] = [];
  for await (const chunk of request) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }

  return chunks.length > 0 ? Buffer.concat(chunks) : undefined;
};

export const createRequestAbortSignal = (
  incomingRequest: IncomingMessage,
  serverResponse: ServerResponse,
): AbortSignal => {
  const controller = new AbortController();
  const abort = () => {
    if (!controller.signal.aborted) {
      controller.abort();
    }
  };
  const abortOnResponseClose = () => {
    if (!serverResponse.writableEnded) {
      abort();
    }
  };
  const cleanup = () => {
    incomingRequest.off("aborted", abort);
    serverResponse.off("close", abortOnResponseClose);
    serverResponse.off("close", cleanup);
    serverResponse.off("finish", cleanup);
  };

  incomingRequest.on("aborted", abort);
  serverResponse.on("close", abortOnResponseClose);
  serverResponse.on("close", cleanup);
  serverResponse.on("finish", cleanup);

  return controller.signal;
};

const toWebRequest = async (
  request: IncomingMessage,
  signal?: AbortSignal,
): Promise<Request> => {
  const host =
    request.headers.host ?? `${DEFAULT_AGENT_HOST}:${DEFAULT_AGENT_PORT}`;
  const url = new URL(request.url ?? "/", `http://${host}`);
  const headers = new Headers();

  for (const [key, value] of Object.entries(request.headers)) {
    if (Array.isArray(value)) {
      for (const item of value) {
        headers.append(key, item);
      }
    } else if (value !== undefined) {
      headers.set(key, value);
    }
  }

  const body = await readIncomingBody(request);

  return new Request(url, {
    body: body ? new Uint8Array(body) : undefined,
    headers,
    method: request.method,
    signal,
  });
};

const sendResponse = async (
  serverResponse: ServerResponse,
  response: Response,
) => {
  serverResponse.statusCode = response.status;
  response.headers.forEach((value, key) => {
    serverResponse.setHeader(key, value);
  });

  if (!response.body) {
    serverResponse.end();
    return;
  }

  const reader = response.body.getReader();
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      serverResponse.write(value);
    }
  } finally {
    serverResponse.end();
    reader.releaseLock();
  }
};

export const createAgentServer = (options: AgentServerOptions = {}) => {
  const runner =
    options.runner ??
    (process.env.NAV_AGENT_MOCK === "1"
      ? new MockAgentRunner()
      : new PiAgentRunner());

  return createServer((incomingRequest, serverResponse) => {
    toWebRequest(
      incomingRequest,
      createRequestAbortSignal(incomingRequest, serverResponse),
    )
      .then((request) => handleAgentRequest(request, runner))
      .then((response) => sendResponse(serverResponse, response))
      .catch((error) => {
        console.error(error);
        serverResponse.statusCode = 500;
        serverResponse.setHeader("content-type", "application/json");
        serverResponse.end(
          JSON.stringify({
            error:
              "Nav request failed. Check the agent server logs for details.",
          }),
        );
      });
  });
};

export const listen = ({
  host = process.env.NAV_AGENT_HOST ?? DEFAULT_AGENT_HOST,
  port = Number(process.env.NAV_AGENT_PORT ?? DEFAULT_AGENT_PORT),
}: {
  host?: string;
  port?: number;
} = {}) => {
  const server = createAgentServer();
  server.listen(port, host, () => {
    console.log(`${SERVICE_NAME} listening on http://${host}:${port}`);
  });
  return server;
};
