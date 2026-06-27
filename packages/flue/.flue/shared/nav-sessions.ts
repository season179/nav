import {
  createSessionStorageKey,
  SUBMISSION_HARNESS_NAME,
  SUBMISSION_SESSION_NAME,
} from "@flue/runtime/adapter";
import type { Context } from "hono";
import {
  ensureNavSessionTable,
  getNavDb,
  getNavStores,
  migrateNavStore,
} from "./nav-db.js";
import {
  ensureNavProjectsReadySync,
  resolveWritableProjectId,
  touchNavProject,
} from "./nav-projects.js";

const AGENT_NAME = "nav";
const SESSION_KEY_PREFIX = "agent-session:";
const MAX_TITLE_LENGTH = 80;
const MAX_PREVIEW_LENGTH = 140;

type NavSessionRow = {
  id: string;
  agent_name: string;
  title: string | null;
  title_source: string;
  pinned: number;
  archived: number;
  project_id: string | null;
  created_at: number;
  last_opened_at: number | null;
  imported_at: number | null;
};

type FlueSessionRow = {
  id: string;
  data: string;
};

type FlueEntryRow = {
  position: number;
  data: string;
};

type FlueSubmissionRow = {
  session_key: string;
  payload: string;
};

export type NavSessionSummary = {
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

let readyPromise: Promise<void> | null = null;

const trimToLength = (value: string, maxLength: number) => {
  const normalized = value.replace(/\s+/g, " ").trim();

  return normalized.length > maxLength
    ? `${normalized.slice(0, maxLength - 1).trim()}...`
    : normalized;
};

const normalizeTitle = (value: unknown) => {
  if (typeof value !== "string") {
    return null;
  }

  const title = trimToLength(value, MAX_TITLE_LENGTH);

  return title.length > 0 ? title : null;
};

const normalizePreview = (value: string | null) =>
  value ? trimToLength(value, MAX_PREVIEW_LENGTH) : null;

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const parseJson = (value: string): Record<string, unknown> | null => {
  try {
    const parsed: unknown = JSON.parse(value);

    return isObject(parsed) ? parsed : null;
  } catch {
    return null;
  }
};

const parseTimestamp = (value: unknown) => {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }

  if (typeof value === "string") {
    const timestamp = Date.parse(value);

    return Number.isFinite(timestamp) ? timestamp : null;
  }

  return null;
};

const extractTextContent = (content: unknown) => {
  if (typeof content === "string") {
    return content;
  }

  if (!Array.isArray(content)) {
    return "";
  }

  return content
    .map((part) => {
      if (!isObject(part) || part.type !== "text") {
        return "";
      }

      return typeof part.text === "string" ? part.text : "";
    })
    .filter(Boolean)
    .join(" ");
};

const extractEntryMessage = (entry: Record<string, unknown>) => {
  const message = entry.message;

  if (!isObject(message)) {
    return null;
  }

  const role = message.role;

  if (role !== "user" && role !== "assistant") {
    return null;
  }

  const text = extractTextContent(message.content);

  return {
    role,
    text,
    timestamp:
      parseTimestamp(entry.timestamp) ?? parseTimestamp(message.timestamp),
  };
};

const parseSessionStorageId = (storageKey: string) => {
  if (!storageKey.startsWith(SESSION_KEY_PREFIX)) {
    return null;
  }

  try {
    const parsed: unknown = JSON.parse(
      storageKey.slice(SESSION_KEY_PREFIX.length),
    );

    if (
      Array.isArray(parsed) &&
      parsed.length === 3 &&
      typeof parsed[0] === "string" &&
      parsed[1] === SUBMISSION_HARNESS_NAME &&
      parsed[2] === SUBMISSION_SESSION_NAME
    ) {
      return parsed[0];
    }
  } catch {
    return null;
  }

  return null;
};

const parseSubmissionAgent = (payload: string) => {
  const parsed = parseJson(payload);

  return typeof parsed?.agent === "string" ? parsed.agent : null;
};

const createStorageKey = (id: string) =>
  createSessionStorageKey(id, SUBMISSION_HARNESS_NAME, SUBMISSION_SESSION_NAME);

const isValidSessionId = (id: string) =>
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id);

const getEntryRows = (storageKey: string) =>
  getNavDb()
    .prepare(
      `SELECT position, data
       FROM flue_session_entries
       WHERE session_id = ?
       ORDER BY position ASC`,
    )
    .all(storageKey) as FlueEntryRow[];

const getSessionData = (storageKey: string) => {
  const row = getNavDb()
    .prepare("SELECT data FROM flue_sessions WHERE id = ?")
    .get(storageKey) as { data: string } | undefined;

  return row ? parseJson(row.data) : null;
};

const summarizeEntries = (entries: FlueEntryRow[]) => {
  let firstUserText: string | null = null;
  let lastPreview: string | null = null;
  let updatedAt: number | null = null;

  for (const row of entries) {
    const entry = parseJson(row.data);
    const message = entry ? extractEntryMessage(entry) : null;

    if (!message) {
      continue;
    }

    if (message.timestamp !== null) {
      updatedAt = message.timestamp;
    }

    const text = message.text.trim();

    if (text.length === 0) {
      continue;
    }

    if (!firstUserText && message.role === "user") {
      firstUserText = text;
    }

    lastPreview = text;
  }

  return {
    firstUserText: normalizeTitle(firstUserText),
    lastPreview: normalizePreview(lastPreview),
    updatedAt,
  };
};

const backfillNavSessions = () => {
  const sql = getNavDb();
  const importedAt = Date.now();
  const sessions = sql
    .prepare("SELECT id, data FROM flue_sessions")
    .all() as FlueSessionRow[];
  const navSessionKeys = new Set(
    (
      sql
        .prepare(
          "SELECT DISTINCT session_key, payload FROM flue_agent_submissions",
        )
        .all() as FlueSubmissionRow[]
    )
      .filter((row) => parseSubmissionAgent(row.payload) === AGENT_NAME)
      .map((row) => row.session_key),
  );
  const hasNavSession = sql.prepare(
    "SELECT 1 FROM nav_sessions WHERE id = ? LIMIT 1",
  );
  const insert = sql.prepare(`
    INSERT INTO nav_sessions (
      id,
      agent_name,
      title,
      title_source,
      pinned,
      archived,
      created_at,
      imported_at
    )
    VALUES (?, ?, ?, 'imported', 0, 0, ?, ?)
    ON CONFLICT(id) DO NOTHING
  `);

  for (const session of sessions) {
    const id = parseSessionStorageId(session.id);

    if (!id || !navSessionKeys.has(session.id) || hasNavSession.get(id)) {
      continue;
    }

    const entries = getEntryRows(session.id);

    if (entries.length === 0) {
      continue;
    }

    const sessionData = parseJson(session.data);
    const summary = summarizeEntries(entries);
    const createdAt =
      parseTimestamp(sessionData?.createdAt) ?? summary.updatedAt ?? importedAt;

    insert.run(
      id,
      AGENT_NAME,
      summary.firstUserText ?? "Untitled chat",
      createdAt,
      importedAt,
    );
  }
};

const initializeNavSessions = async () => {
  await migrateNavStore();
  ensureNavSessionTable();
  backfillNavSessions();
  ensureNavProjectsReadySync();
};

export const ensureNavSessionsReady = () => {
  readyPromise ??= initializeNavSessions().catch((error: unknown) => {
    readyPromise = null;
    throw error;
  });

  return readyPromise;
};

const listNavSessions = async (includeArchived: boolean) => {
  await ensureNavSessionsReady();

  const rows = getNavDb()
    .prepare(
      `SELECT
        s.id,
        s.agent_name,
        s.title,
        s.title_source,
        s.pinned,
        s.archived,
        s.project_id,
        s.created_at,
        s.last_opened_at,
        s.imported_at
       FROM nav_sessions s
       LEFT JOIN nav_projects p ON p.id = s.project_id
       WHERE
        s.agent_name = ?
        AND (? = 1 OR s.archived = 0)
        AND (s.project_id IS NULL OR p.archived = 0)`,
    )
    .all(AGENT_NAME, includeArchived ? 1 : 0) as NavSessionRow[];
  const sessions: NavSessionSummary[] = [];

  for (const row of rows) {
    const storageKey = createStorageKey(row.id);
    const sessionData = getSessionData(storageKey);

    if (!sessionData) {
      continue;
    }

    const entries = getEntryRows(storageKey);
    const summary = summarizeEntries(entries);
    const updatedAt =
      summary.updatedAt ??
      parseTimestamp(sessionData?.updatedAt) ??
      row.created_at;

    sessions.push({
      id: row.id,
      title: row.title ?? summary.firstUserText,
      titleSource: row.title_source,
      pinned: row.pinned === 1,
      archived: row.archived === 1,
      projectId: row.project_id ?? "",
      createdAt: row.created_at,
      updatedAt,
      lastPreview: summary.lastPreview,
    });
  }

  return sessions.sort((left, right) => {
    if (left.pinned !== right.pinned) {
      return left.pinned ? -1 : 1;
    }

    return right.updatedAt - left.updatedAt;
  });
};

const createOrAdoptNavSession = async (
  id: string,
  title: string | null,
  projectId: string,
) => {
  await ensureNavSessionsReady();

  const now = Date.now();

  getNavDb()
    .prepare(
      `INSERT INTO nav_sessions (
        id,
        agent_name,
        title,
        title_source,
        pinned,
        archived,
        project_id,
        created_at,
        last_opened_at
       )
       VALUES (?, ?, ?, 'first-message', 0, 0, ?, ?, ?)
       ON CONFLICT(id) DO UPDATE SET
        title = CASE
          WHEN nav_sessions.title IS NULL AND excluded.title IS NOT NULL
            THEN excluded.title
          ELSE nav_sessions.title
        END,
        title_source = CASE
          WHEN nav_sessions.title IS NULL AND excluded.title IS NOT NULL
            THEN excluded.title_source
          ELSE nav_sessions.title_source
        END,
        project_id = COALESCE(nav_sessions.project_id, excluded.project_id),
        last_opened_at = excluded.last_opened_at`,
    )
    .run(id, AGENT_NAME, title, projectId, now, now);

  touchNavProject(projectId);
};

const updateNavSession = async (
  id: string,
  input: { title?: string; pinned?: boolean; archived?: boolean },
) => {
  await ensureNavSessionsReady();

  const sets: string[] = [];
  const values: (string | number)[] = [];

  if (input.title !== undefined) {
    sets.push("title = ?", "title_source = 'manual'");
    values.push(input.title);
  }

  if (input.pinned !== undefined) {
    sets.push("pinned = ?");
    values.push(input.pinned ? 1 : 0);
  }

  if (input.archived !== undefined) {
    sets.push("archived = ?");
    values.push(input.archived ? 1 : 0);
  }

  if (sets.length === 0) {
    return;
  }

  getNavDb()
    .prepare(`UPDATE nav_sessions SET ${sets.join(", ")} WHERE id = ?`)
    .run(...values, id);
};

const deleteNavSession = async (id: string) => {
  await ensureNavSessionsReady();

  const storageKey = createStorageKey(id);
  const stores = await getNavStores();

  await stores.executionStore.submissions.deleteSession(storageKey, () =>
    stores.executionStore.sessions.delete(storageKey),
  );

  getNavDb().prepare("DELETE FROM nav_sessions WHERE id = ?").run(id);
};

const readJsonObject = async (c: Context) => {
  try {
    const body: unknown = await c.req.json();

    return isObject(body) ? body : null;
  } catch {
    return null;
  }
};

export const handleListNavSessions = async (c: Context) => {
  const includeArchived = c.req.query("archived") === "true";
  const sessions = await listNavSessions(includeArchived);

  return c.json({ sessions });
};

export const handleCreateNavSession = async (c: Context) => {
  const body = await readJsonObject(c);
  const id = typeof body?.id === "string" ? body.id.trim() : "";

  if (!isValidSessionId(id)) {
    return c.json({ error: "invalid_session_id" }, 400);
  }

  const project = resolveWritableProjectId(body?.projectId);

  if (!project.ok) {
    return c.json(
      { error: project.error, message: project.message },
      project.status,
    );
  }

  await createOrAdoptNavSession(
    id,
    normalizeTitle(body?.title),
    project.projectId,
  );

  return c.json({ ok: true }, 201);
};

export const handleUpdateNavSession = async (c: Context) => {
  const id = c.req.param("id") ?? "";

  if (!isValidSessionId(id)) {
    return c.json({ error: "invalid_session_id" }, 400);
  }

  const body = await readJsonObject(c);

  if (!body) {
    return c.json({ error: "invalid_json" }, 400);
  }

  const input: { title?: string; pinned?: boolean; archived?: boolean } = {};

  if ("title" in body) {
    const title = normalizeTitle(body.title);

    if (!title) {
      return c.json({ error: "invalid_title" }, 400);
    }

    input.title = title;
  }

  if ("pinned" in body) {
    if (typeof body.pinned !== "boolean") {
      return c.json({ error: "invalid_pinned" }, 400);
    }

    input.pinned = body.pinned;
  }

  if ("archived" in body) {
    if (typeof body.archived !== "boolean") {
      return c.json({ error: "invalid_archived" }, 400);
    }

    input.archived = body.archived;
  }

  await updateNavSession(id, input);

  return c.json({ ok: true });
};

export const handleDeleteNavSession = async (c: Context) => {
  const id = c.req.param("id") ?? "";

  if (!isValidSessionId(id)) {
    return c.json({ error: "invalid_session_id" }, 400);
  }

  try {
    await deleteNavSession(id);
  } catch (error) {
    return c.json(
      {
        error: "session_delete_failed",
        message:
          error instanceof Error
            ? error.message
            : "Unable to delete this session.",
      },
      409,
    );
  }

  return c.json({ ok: true });
};
