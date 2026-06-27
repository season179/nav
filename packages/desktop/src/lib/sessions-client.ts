import type { FlueConnection } from "@/lib/flue-connection";

export type NavSession = {
  id: string;
  title: string | null;
  titleSource: string;
  pinned: boolean;
  archived: boolean;
  createdAt: number;
  updatedAt: number;
  lastPreview: string | null;
};

type SessionsResponse = {
  sessions: NavSession[];
};

type RequestOptions = {
  body?: unknown;
  method?: "GET" | "POST" | "PATCH" | "DELETE";
};

export class SessionsClientError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SessionsClientError";
  }
}

const readErrorMessage = async (response: Response) => {
  try {
    const body: unknown = await response.json();

    if (
      typeof body === "object" &&
      body !== null &&
      "message" in body &&
      typeof body.message === "string"
    ) {
      return body.message;
    }

    if (
      typeof body === "object" &&
      body !== null &&
      "error" in body &&
      typeof body.error === "string"
    ) {
      return body.error;
    }
  } catch {
    // Fall through to the HTTP status text.
  }

  return response.statusText || "Session request failed.";
};

export function createSessionsClient(connection: FlueConnection) {
  const request = async <T>(path: string, options: RequestOptions = {}) => {
    const response = await fetch(new URL(path, connection.baseUrl), {
      body:
        options.body === undefined ? undefined : JSON.stringify(options.body),
      headers: {
        Authorization: `Bearer ${connection.token}`,
        ...(options.body === undefined
          ? {}
          : { "Content-Type": "application/json" }),
      },
      method: options.method ?? "GET",
    });

    if (!response.ok) {
      throw new SessionsClientError(await readErrorMessage(response));
    }

    return (await response.json()) as T;
  };

  return {
    async createSession(id: string, title: string) {
      await request<{ ok: true }>("/api/sessions", {
        body: { id, title },
        method: "POST",
      });
    },
    async deleteSession(id: string) {
      await request<{ ok: true }>(`/api/sessions/${id}`, {
        method: "DELETE",
      });
    },
    async listSessions() {
      const response = await request<SessionsResponse>("/api/sessions");

      return response.sessions;
    },
    async renameSession(id: string, title: string) {
      await request<{ ok: true }>(`/api/sessions/${id}`, {
        body: { title },
        method: "PATCH",
      });
    },
  };
}
