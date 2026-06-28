import type { Context } from "hono";
import { ensureMessageClassificationsReady, getNavDb } from "./nav-db.js";
import {
  normalizeRequestClassification,
  type RequestClassification,
  type RequestDifficulty,
} from "./request-classifier.js";

const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

type ClassificationRow = {
  created_at: number;
  difficulty: string;
  is_planning: number;
  message_id: string;
  session_id: string;
};

type ClassificationWorkflowResponse = {
  result?: unknown;
};

export type MessageClassification = RequestClassification & {
  messageId: string;
};

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const isValidUuid = (value: string) => UUID_PATTERN.test(value);

const isValidMessageId = (value: string) =>
  value.length > 0 && value.length <= 200;

const isRequestDifficulty = (value: string): value is RequestDifficulty =>
  value === "low" || value === "medium" || value === "high";

const normalizeText = (value: unknown) =>
  typeof value === "string" ? value.replace(/\s+/g, " ").trim() : "";

const readJsonObject = async (c: Context) => {
  try {
    const body: unknown = await c.req.json();

    return isObject(body) ? body : null;
  } catch {
    return null;
  }
};

const rowToClassification = (
  row: ClassificationRow,
): MessageClassification | null => {
  if (!isRequestDifficulty(row.difficulty)) {
    return null;
  }

  return {
    difficulty: row.difficulty,
    isPlanning: row.is_planning === 1,
    messageId: row.message_id,
  };
};

const selectClassification = (sessionId: string, messageId: string) => {
  const row = getNavDb()
    .prepare(
      `SELECT
        session_id,
        message_id,
        is_planning,
        difficulty,
        created_at
       FROM nav_message_classifications
       WHERE session_id = ? AND message_id = ?
       LIMIT 1`,
    )
    .get(sessionId, messageId) as ClassificationRow | undefined;

  return row ? rowToClassification(row) : null;
};

const listClassifications = (sessionId: string) =>
  (
    getNavDb()
      .prepare(
        `SELECT
          session_id,
          message_id,
          is_planning,
          difficulty,
          created_at
         FROM nav_message_classifications
         WHERE session_id = ?
         ORDER BY created_at ASC`,
      )
      .all(sessionId) as ClassificationRow[]
  )
    .map(rowToClassification)
    .filter((row): row is MessageClassification => row !== null);

const insertClassification = (
  sessionId: string,
  messageId: string,
  classification: RequestClassification,
) => {
  getNavDb()
    .prepare(
      `INSERT INTO nav_message_classifications (
        session_id,
        message_id,
        is_planning,
        difficulty,
        created_at
       )
       VALUES (?, ?, ?, ?, ?)
       ON CONFLICT(session_id, message_id) DO UPDATE SET
        is_planning = excluded.is_planning,
        difficulty = excluded.difficulty`,
    )
    .run(
      sessionId,
      messageId,
      classification.isPlanning ? 1 : 0,
      classification.difficulty,
      Date.now(),
    );
};

const requestMessageClassification = async (
  input: { priorAssistant?: string; text: string },
  signal?: AbortSignal,
) => {
  const port = process.env.NAV_FLUE_PORT;
  const token = process.env.NAV_DESKTOP_TOKEN;

  if (!port || !token) {
    throw new Error("NAV_FLUE_PORT / NAV_DESKTOP_TOKEN not set");
  }

  const res = await fetch(
    `http://127.0.0.1:${port}/api/workflows/request-classifier?wait=result`,
    {
      body: JSON.stringify(input),
      headers: {
        authorization: `Bearer ${token}`,
        "content-type": "application/json",
      },
      method: "POST",
      signal,
    },
  );

  if (!res.ok) {
    throw new Error(`classification failed: ${res.status} ${await res.text()}`);
  }

  const json = (await res.json()) as ClassificationWorkflowResponse;
  const classification = normalizeRequestClassification(json.result);

  if (!classification) {
    throw new Error("Classifier returned an invalid result.");
  }

  return classification;
};

export const handleListNavSessionClassifications = async (c: Context) => {
  const sessionId = c.req.param("id") ?? "";

  if (!isValidUuid(sessionId)) {
    return c.json({ error: "invalid_session_id" }, 400);
  }

  ensureMessageClassificationsReady();

  return c.json({ classifications: listClassifications(sessionId) });
};

export const handleClassifyNavSessionMessage = async (c: Context) => {
  const sessionId = c.req.param("id") ?? "";

  if (!isValidUuid(sessionId)) {
    return c.json({ error: "invalid_session_id" }, 400);
  }

  const body = await readJsonObject(c);
  const messageId = normalizeText(body?.messageId);

  if (!isValidMessageId(messageId)) {
    return c.json({ error: "invalid_message_id" }, 400);
  }

  const text = normalizeText(body?.text);

  ensureMessageClassificationsReady();

  const existing = selectClassification(sessionId, messageId);

  if (existing) {
    return c.json(existing);
  }

  if (!text) {
    return c.body(null, 204);
  }

  try {
    const priorAssistant = normalizeText(body?.priorAssistant);
    const classification = await requestMessageClassification(
      {
        ...(priorAssistant ? { priorAssistant } : {}),
        text,
      },
      c.req.raw.signal,
    );

    insertClassification(sessionId, messageId, classification);

    return c.json({
      ...classification,
      messageId,
    });
  } catch (error) {
    return c.json(
      {
        error: "message_classification_failed",
        message:
          error instanceof Error
            ? error.message
            : "Unable to classify this message.",
      },
      503,
    );
  }
};
