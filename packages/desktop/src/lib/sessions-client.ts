import type { FlueConnection } from "@/lib/flue-connection";

export type NavSession = {
  id: string;
  title: string | null;
  titleSource: string;
  pinned: boolean;
  archived: boolean;
  projectId: string;
  createdAt: number;
  updatedAt: number;
  lastPreview: string | null;
};

export type MessageDifficulty = "low" | "medium" | "high";

export type MessageClassification = {
  difficulty: MessageDifficulty;
  isPlanning: boolean;
  messageId: string;
};

type SessionsResponse = {
  sessions: NavSession[];
};

type ClassificationsResponse = {
  classifications: MessageClassification[];
};

type GenerateTitleResponse = {
  generated: boolean;
  ok: true;
  title: string | null;
  titleSource: string;
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

    if (response.status === 204) {
      return null as T;
    }

    return (await response.json()) as T;
  };

  return {
    async classifyMessage(
      id: string,
      input: { messageId: string; priorAssistant?: string; text: string },
    ) {
      return request<MessageClassification | null>(
        `/api/sessions/${id}/classify`,
        {
          body: input,
          method: "POST",
        },
      );
    },
    async createSession(
      id: string,
      title: string | null,
      projectId: string | null,
    ) {
      await request<{ ok: true }>("/api/sessions", {
        body: { id, projectId, ...(title === null ? {} : { title }) },
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
    async listClassifications(id: string) {
      const response = await request<ClassificationsResponse>(
        `/api/sessions/${id}/classifications`,
      );

      return response.classifications;
    },
    async generateSessionTitle(id: string) {
      return request<GenerateTitleResponse>(
        `/api/sessions/${id}/title/generate`,
        {
          method: "POST",
        },
      );
    },
    async renameSession(id: string, title: string) {
      await request<{ ok: true }>(`/api/sessions/${id}`, {
        body: { title },
        method: "PATCH",
      });
    },
  };
}

export type SessionsClient = ReturnType<typeof createSessionsClient>;
