import { randomBytes } from "node:crypto";
import { existsSync, realpathSync, statSync } from "node:fs";
import { basename, join, normalize, resolve } from "node:path";
import type { Context } from "hono";
import { getWorkspaceRoot } from "./codex.js";
import {
  ensureNavProjectTable,
  ensureNavSessionTable,
  getNavDb,
} from "./nav-db.js";

const MAX_PROJECT_NAME_LENGTH = 80;
const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

type NavProjectRow = {
  id: string;
  name: string;
  path: string;
  display_path: string | null;
  is_default: number;
  archived: number;
  created_at: number;
  last_opened_at: number | null;
};

type SessionProjectRow = {
  project_id: string | null;
  id: string | null;
  name: string | null;
  path: string | null;
  display_path: string | null;
  is_default: number | null;
  archived: number | null;
};

export type NavProjectSummary = {
  id: string;
  name: string;
  path: string;
  displayPath: string | null;
  isDefault: boolean;
  archived: boolean;
  available: boolean;
  createdAt: number;
  lastOpenedAt: number | null;
};

export type ResolvedSessionProject = {
  id: string;
  name: string;
  path: string;
  isDefault: boolean;
  archived: boolean;
  available: boolean;
};

type ProjectIdResolution =
  | { ok: true; projectId: string }
  | { ok: false; error: string; message: string; status: 400 | 404 };

let readyPromise: Promise<void> | null = null;

const formatUuidBytes = (bytes: Uint8Array) =>
  [...bytes]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("")
    .replace(/^(.{8})(.{4})(.{4})(.{4})(.{12})$/, "$1-$2-$3-$4-$5");

const createUuidV7 = () => {
  const bytes = randomBytes(16);
  const timestamp = BigInt(Date.now());

  bytes[0] = Number((timestamp >> 40n) & 0xffn);
  bytes[1] = Number((timestamp >> 32n) & 0xffn);
  bytes[2] = Number((timestamp >> 24n) & 0xffn);
  bytes[3] = Number((timestamp >> 16n) & 0xffn);
  bytes[4] = Number((timestamp >> 8n) & 0xffn);
  bytes[5] = Number(timestamp & 0xffn);
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x70;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;

  return formatUuidBytes(bytes);
};

const trimToLength = (value: string, maxLength: number) => {
  const normalized = value.replace(/\s+/g, " ").trim();

  return normalized.length > maxLength
    ? `${normalized.slice(0, maxLength - 1).trim()}...`
    : normalized;
};

const normalizeProjectName = (value: unknown) => {
  if (typeof value !== "string") {
    return null;
  }

  const name = trimToLength(value, MAX_PROJECT_NAME_LENGTH);

  return name.length > 0 ? name : null;
};

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const readJsonObject = async (c: Context) => {
  try {
    const body: unknown = await c.req.json();

    return isObject(body) ? body : null;
  } catch {
    return null;
  }
};

const isValidUuid = (id: string) => UUID_PATTERN.test(id);

const isDirectoryAvailable = (projectPath: string) => {
  try {
    return existsSync(projectPath) && statSync(projectPath).isDirectory();
  } catch {
    return false;
  }
};

const canonicalizeExistingDirectory = (input: unknown) => {
  if (typeof input !== "string") {
    return null;
  }

  const rawPath = input.trim();

  if (!rawPath) {
    return null;
  }

  const resolved = resolve(rawPath);
  const stats = statSync(resolved);

  if (!stats.isDirectory()) {
    throw new Error("Selected path is not a directory.");
  }

  const canonicalPath = normalize(realpathSync(resolved));

  return {
    path: canonicalPath,
    displayPath: normalize(resolved),
  };
};

const canonicalizeWorkspaceRoot = () => {
  const root = resolve(getWorkspaceRoot());

  try {
    return normalize(realpathSync(root));
  } catch {
    return normalize(root);
  }
};

const isDefaultProjectPath = (projectPath: string) =>
  normalize(projectPath) === canonicalizeWorkspaceRoot();

const selectProjectByPath = (projectPath: string) =>
  getNavDb()
    .prepare(
      `SELECT
        id,
        name,
        path,
        display_path,
        is_default,
        archived,
        created_at,
        last_opened_at
       FROM nav_projects
       WHERE path = ? COLLATE NOCASE
       LIMIT 1`,
    )
    .get(projectPath) as NavProjectRow | undefined;

const selectProjectById = (id: string) =>
  getNavDb()
    .prepare(
      `SELECT
        id,
        name,
        path,
        display_path,
        is_default,
        archived,
        created_at,
        last_opened_at
       FROM nav_projects
       WHERE id = ?
       LIMIT 1`,
    )
    .get(id) as NavProjectRow | undefined;

const selectDefaultProject = () =>
  selectProjectByPath(canonicalizeWorkspaceRoot()) ??
  (getNavDb()
    .prepare(
      `SELECT
         id,
         name,
         path,
         display_path,
         is_default,
         archived,
         created_at,
         last_opened_at
       FROM nav_projects
       WHERE is_default = 1
       ORDER BY created_at ASC
       LIMIT 1`,
    )
    .get() as NavProjectRow | undefined);

const serializeProject = (row: NavProjectRow): NavProjectSummary => ({
  id: row.id,
  name: row.name,
  path: row.path,
  displayPath: row.display_path,
  isDefault: isDefaultProjectPath(row.path),
  archived: row.archived === 1,
  available: isDirectoryAvailable(row.path),
  createdAt: row.created_at,
  lastOpenedAt: row.last_opened_at,
});

const missingProjectAfterWrite = (c: Context) =>
  c.json(
    {
      error: "project_write_failed",
      message: "Project was saved, but could not be read back.",
    },
    500,
  );

const markOnlyDefaultProject = (id: string) => {
  getNavDb()
    .prepare(
      `UPDATE nav_projects
       SET is_default = CASE WHEN id = ? THEN 1 ELSE 0 END`,
    )
    .run(id);
};

const ensureDefaultProject = () => {
  const now = Date.now();
  const root = canonicalizeWorkspaceRoot();
  const existing = selectProjectByPath(root);

  if (existing) {
    getNavDb()
      .prepare(
        `UPDATE nav_projects
         SET is_default = 1,
             archived = 0,
             display_path = COALESCE(display_path, ?),
             last_opened_at = COALESCE(last_opened_at, ?)
         WHERE id = ?`,
      )
      .run(root, now, existing.id);

    markOnlyDefaultProject(existing.id);
    return existing.id;
  }

  const id = createUuidV7();

  getNavDb()
    .prepare(
      `INSERT INTO nav_projects (
        id,
        name,
        path,
        display_path,
        is_default,
        archived,
        created_at,
        last_opened_at
       )
       VALUES (?, ?, ?, ?, 1, 0, ?, ?)`,
    )
    .run(id, basename(root) || "Nav", root, root, now, now);

  markOnlyDefaultProject(id);
  return id;
};

const backfillDefaultProjectSessions = (defaultProjectId: string) => {
  getNavDb()
    .prepare(
      `UPDATE nav_sessions
       SET project_id = ?
       WHERE project_id IS NULL`,
    )
    .run(defaultProjectId);
};

export const ensureNavProjectsReadySync = () => {
  ensureNavSessionTable();
  ensureNavProjectTable();

  const defaultProjectId = ensureDefaultProject();
  backfillDefaultProjectSessions(defaultProjectId);
};

export const ensureNavProjectsReady = () => {
  readyPromise ??= Promise.resolve()
    .then(() => {
      ensureNavProjectsReadySync();
    })
    .catch((error: unknown) => {
      readyPromise = null;
      throw error;
    });

  return readyPromise;
};

const getDefaultProject = () => {
  ensureNavProjectsReadySync();

  const project = selectDefaultProject();

  if (!project) {
    throw new Error("Default Nav project is not available.");
  }

  return project;
};

export const resolveWritableProjectId = (
  value: unknown,
): ProjectIdResolution => {
  ensureNavProjectsReadySync();

  if (value == null || value === "") {
    return { ok: true, projectId: getDefaultProject().id };
  }

  if (typeof value !== "string" || !isValidUuid(value)) {
    return {
      ok: false,
      error: "invalid_project_id",
      message: "Project id is invalid.",
      status: 400,
    };
  }

  const project = selectProjectById(value);

  if (!project || project.archived === 1) {
    return {
      ok: false,
      error: "project_not_found",
      message: "Project was not found.",
      status: 404,
    };
  }

  return { ok: true, projectId: project.id };
};

export const touchNavProject = (id: string) => {
  getNavDb()
    .prepare("UPDATE nav_projects SET last_opened_at = ? WHERE id = ?")
    .run(Date.now(), id);
};

export const listNavProjectPathsForWorktreePrune = () => {
  ensureNavProjectsReadySync();

  return (
    getNavDb().prepare("SELECT path FROM nav_projects").all() as {
      path: string;
    }[]
  ).map((row) => row.path);
};

export const resolveSessionProject = (
  sessionId: string,
): ResolvedSessionProject => {
  ensureNavProjectsReadySync();

  const defaultProject = getDefaultProject();
  const row = getNavDb()
    .prepare(
      `SELECT
        s.project_id,
        p.id,
        p.name,
        p.path,
        p.display_path,
        p.is_default,
        p.archived
       FROM nav_sessions s
       LEFT JOIN nav_projects p ON p.id = s.project_id
       WHERE s.id = ?
       LIMIT 1`,
    )
    .get(sessionId) as SessionProjectRow | undefined;

  if (!row?.project_id) {
    return {
      id: defaultProject.id,
      name: defaultProject.name,
      path: defaultProject.path,
      isDefault: true,
      archived: defaultProject.archived === 1,
      available: isDirectoryAvailable(defaultProject.path),
    };
  }

  if (!row.id || !row.path || !row.name) {
    const missingPath = join(
      canonicalizeWorkspaceRoot(),
      ".nav-missing-project",
      row.project_id,
    );

    return {
      id: row.project_id,
      name: "Missing project",
      path: missingPath,
      isDefault: false,
      archived: false,
      available: false,
    };
  }

  return {
    id: row.id,
    name: row.name,
    path: row.path,
    isDefault: isDefaultProjectPath(row.path),
    archived: row.archived === 1,
    available: isDirectoryAvailable(row.path),
  };
};

export const handleListNavProjects = async (c: Context) => {
  await ensureNavProjectsReady();

  const projects = (
    getNavDb()
      .prepare(
        `SELECT
          id,
          name,
          path,
          display_path,
          is_default,
          archived,
          created_at,
          last_opened_at
         FROM nav_projects
         WHERE archived = 0
         ORDER BY COALESCE(last_opened_at, created_at) DESC, created_at DESC`,
      )
      .all() as NavProjectRow[]
  ).map(serializeProject);

  return c.json({ projects });
};

export const handleCreateNavProject = async (c: Context) => {
  const body = await readJsonObject(c);

  if (!body) {
    return c.json({ error: "invalid_json" }, 400);
  }

  let canonical: { path: string; displayPath: string };

  try {
    const nextCanonical = canonicalizeExistingDirectory(body.path);

    if (!nextCanonical) {
      return c.json({ error: "invalid_path" }, 400);
    }

    canonical = nextCanonical;
  } catch (error) {
    return c.json(
      {
        error: "invalid_path",
        message:
          error instanceof Error
            ? error.message
            : "Selected path is not a directory.",
      },
      400,
    );
  }

  await ensureNavProjectsReady();

  const now = Date.now();
  const existing = selectProjectByPath(canonical.path);
  const requestedName = normalizeProjectName(body.name);
  const name =
    requestedName ??
    trimToLength(
      basename(canonical.path) || canonical.path,
      MAX_PROJECT_NAME_LENGTH,
    );

  if (existing) {
    getNavDb()
      .prepare(
        `UPDATE nav_projects
         SET archived = 0,
             name = CASE WHEN ? IS NULL THEN name ELSE ? END,
             display_path = ?,
             last_opened_at = ?
         WHERE id = ?`,
      )
      .run(
        requestedName,
        requestedName,
        canonical.displayPath,
        now,
        existing.id,
      );

    const project = selectProjectById(existing.id);

    if (!project) {
      return missingProjectAfterWrite(c);
    }

    return c.json({ project: serializeProject(project) });
  }

  const id = createUuidV7();

  getNavDb()
    .prepare(
      `INSERT INTO nav_projects (
        id,
        name,
        path,
        display_path,
        is_default,
        archived,
        created_at,
        last_opened_at
       )
       VALUES (?, ?, ?, ?, 0, 0, ?, ?)`,
    )
    .run(id, name, canonical.path, canonical.displayPath, now, now);

  const project = selectProjectById(id);

  if (!project) {
    return missingProjectAfterWrite(c);
  }

  return c.json({ project: serializeProject(project) }, 201);
};

export const handleUpdateNavProject = async (c: Context) => {
  await ensureNavProjectsReady();

  const id = c.req.param("id") ?? "";

  if (!isValidUuid(id)) {
    return c.json({ error: "invalid_project_id" }, 400);
  }

  const project = selectProjectById(id);

  if (!project) {
    return c.json({ error: "project_not_found" }, 404);
  }

  const body = await readJsonObject(c);

  if (!body) {
    return c.json({ error: "invalid_json" }, 400);
  }

  const sets: string[] = [];
  const values: (string | number)[] = [];

  if ("name" in body) {
    const name = normalizeProjectName(body.name);

    if (!name) {
      return c.json({ error: "invalid_name" }, 400);
    }

    sets.push("name = ?");
    values.push(name);
  }

  if ("archived" in body) {
    if (typeof body.archived !== "boolean") {
      return c.json({ error: "invalid_archived" }, 400);
    }

    if (isDefaultProjectPath(project.path) && body.archived) {
      return c.json(
        {
          error: "default_project_not_removable",
          message: "The default Nav project cannot be removed.",
        },
        400,
      );
    }

    sets.push("archived = ?");
    values.push(body.archived ? 1 : 0);
  }

  if (sets.length === 0) {
    return c.json({ project: serializeProject(project) });
  }

  getNavDb()
    .prepare(`UPDATE nav_projects SET ${sets.join(", ")} WHERE id = ?`)
    .run(...values, id);

  const updated = selectProjectById(id);

  if (!updated) {
    return missingProjectAfterWrite(c);
  }

  return c.json({ project: serializeProject(updated) });
};

export const handleDeleteNavProject = async (c: Context) => {
  await ensureNavProjectsReady();

  const id = c.req.param("id") ?? "";

  if (!isValidUuid(id)) {
    return c.json({ error: "invalid_project_id" }, 400);
  }

  const project = selectProjectById(id);

  if (!project) {
    return c.json({ error: "project_not_found" }, 404);
  }

  if (isDefaultProjectPath(project.path)) {
    return c.json(
      {
        error: "default_project_not_removable",
        message: "The default Nav project cannot be removed.",
      },
      400,
    );
  }

  getNavDb()
    .prepare("UPDATE nav_projects SET archived = 1 WHERE id = ?")
    .run(id);

  return c.json({ ok: true });
};
