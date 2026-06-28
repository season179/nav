import type { FlueConnection } from "@/lib/flue-connection";

export type NavProject = {
  id: string;
  name: string;
  path: string;
  displayPath: string | null;
  isDefault: boolean;
  archived: boolean;
  available: boolean;
  modelSpec: string | null;
  autoApproveEdits: boolean;
  color: string | null;
  icon: string | null;
  sortOrder: number | null;
  createdAt: number;
  lastOpenedAt: number | null;
};

export type ProjectUpdate = {
  autoApproveEdits?: boolean;
  color?: string | null;
  icon?: string | null;
  modelSpec?: string | null;
  name?: string;
  path?: string;
};

type ProjectsResponse = {
  projects: NavProject[];
};

type ProjectResponse = {
  project: NavProject;
};

type RequestOptions = {
  body?: unknown;
  method?: "GET" | "POST" | "PATCH" | "DELETE";
};

export class ProjectsClientError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProjectsClientError";
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

  return response.statusText || "Project request failed.";
};

export function createProjectsClient(connection: FlueConnection) {
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
      throw new ProjectsClientError(await readErrorMessage(response));
    }

    return (await response.json()) as T;
  };

  return {
    async createProject(path: string) {
      const response = await request<ProjectResponse>("/api/projects", {
        body: { path },
        method: "POST",
      });

      return response.project;
    },
    async listProjects(includeArchived = false) {
      const response = await request<ProjectsResponse>(
        includeArchived ? "/api/projects?archived=true" : "/api/projects",
      );

      return response.projects;
    },
    async removeProject(id: string) {
      await request<{ ok: true }>(`/api/projects/${id}`, {
        method: "DELETE",
      });
    },
    async relocateProject(id: string, path: string) {
      const response = await request<ProjectResponse>(`/api/projects/${id}`, {
        body: { path },
        method: "PATCH",
      });

      return response.project;
    },
    async reorderProjects(projectIds: string[]) {
      await request<{ ok: true }>("/api/projects/order", {
        body: { projectIds },
        method: "PATCH",
      });
    },
    async restoreProject(id: string) {
      const response = await request<ProjectResponse>(`/api/projects/${id}`, {
        body: { archived: false },
        method: "PATCH",
      });

      return response.project;
    },
    async renameProject(id: string, name: string) {
      const response = await request<ProjectResponse>(`/api/projects/${id}`, {
        body: { name },
        method: "PATCH",
      });

      return response.project;
    },
    async updateProject(id: string, update: ProjectUpdate) {
      const response = await request<ProjectResponse>(`/api/projects/${id}`, {
        body: update,
        method: "PATCH",
      });

      return response.project;
    },
  };
}
