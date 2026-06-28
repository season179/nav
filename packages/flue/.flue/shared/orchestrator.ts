import { randomBytes } from "node:crypto";
import { consultAgentWithId, createDelegationId } from "./delegation.js";
import type { GitContext } from "./git-context.js";
import { ensureOrchestratorReady, getNavDb } from "./nav-db.js";
import type { ResolvedSessionProject } from "./nav-projects.js";
import { requestMessageClassification } from "./request-classifications.js";
import type { RequestDifficulty } from "./request-classifier.js";

type OrchestratorMode = "direct" | "panel";
type OrchestratorTurnStatus = "complete" | "failed" | "partial" | "pending";
type DelegateAgent = "glm" | "deepseek-pro";
type DelegateStatus = "complete" | "failed" | "timeout";

type OrchestratorStateRow = {
  active: number;
  cleared_at: number | null;
  project_id: string | null;
  session_id: string;
  started_at: number | null;
  thread_id: string | null;
  updated_at: number;
};

type OrchestratorTurnRow = {
  completed_at: number | null;
  created_at: number;
  difficulty: string | null;
  error: string | null;
  id: string;
  is_planning: number;
  mode: string;
  project_id: string | null;
  request_text: string;
  session_id: string;
  status: string;
  thread_id: string | null;
};

type DelegateResultRow = {
  agent: string;
  agent_session_id: string;
  answer: string | null;
  completed_at: number | null;
  error: string | null;
  started_at: number;
  status: string;
  turn_id: string;
  worktree: string | null;
};

export type OrchestratorDelegateResult = {
  agent: DelegateAgent;
  answer: string | null;
  error: string | null;
  status: DelegateStatus;
  worktree: string | null;
};

export type OrchestratorTurnContext = {
  active: boolean;
  delegateResults: OrchestratorDelegateResult[];
  difficulty: RequestDifficulty | null;
  error: string | null;
  isPlanning: boolean;
  mode: OrchestratorMode;
  status: OrchestratorTurnStatus;
  threadId: string | null;
  turnId: string;
};

const PANEL_AGENTS = ["glm", "deepseek-pro"] as const;
const MAX_REQUEST_TEXT_CHARS = 12_000;
const MAX_DELEGATE_ANSWER_CHARS = 16_000;
const DEFAULT_ORCHESTRATOR_DELEGATE_TIMEOUT_MS = 15 * 60 * 1000;

class DelegateTimeoutError extends Error {
  constructor(agent: string) {
    super(`${agent} timed out.`);
    this.name = "DelegateTimeoutError";
  }
}

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

const normalizeText = (value: string) => value.replace(/\s+/g, " ").trim();

const clipText = (value: string, maxChars: number) => {
  const normalized = normalizeText(value);

  return normalized.length > maxChars
    ? `${normalized.slice(0, maxChars - 1).trim()}...`
    : normalized;
};

const errorMessage = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

const parseDelegateTimeoutMs = () => {
  const configured = Number(process.env.NAV_ORCHESTRATOR_DELEGATE_TIMEOUT_MS);

  return Number.isSafeInteger(configured) && configured > 0
    ? configured
    : DEFAULT_ORCHESTRATOR_DELEGATE_TIMEOUT_MS;
};

const isRequestDifficulty = (
  value: string | null,
): value is RequestDifficulty =>
  value === "low" || value === "medium" || value === "high";

const isTurnStatus = (value: string): value is OrchestratorTurnStatus =>
  value === "complete" ||
  value === "failed" ||
  value === "partial" ||
  value === "pending";

const isTurnMode = (value: string): value is OrchestratorMode =>
  value === "direct" || value === "panel";

const isDelegateAgent = (value: string): value is DelegateAgent =>
  value === "glm" || value === "deepseek-pro";

const isDelegateStatus = (value: string): value is DelegateStatus =>
  value === "complete" || value === "failed" || value === "timeout";

const selectState = (sessionId: string) => {
  ensureOrchestratorReady();

  return getNavDb()
    .prepare(
      `SELECT
        session_id,
        project_id,
        active,
        thread_id,
        started_at,
        updated_at,
        cleared_at
       FROM nav_orchestrator_state
       WHERE session_id = ?
       LIMIT 1`,
    )
    .get(sessionId) as OrchestratorStateRow | undefined;
};

const upsertState = (input: {
  active: boolean;
  projectId: string;
  sessionId: string;
  threadId: string | null;
}) => {
  ensureOrchestratorReady();

  const now = Date.now();
  const existing = selectState(input.sessionId);
  const startedAt = input.active
    ? existing?.active === 1
      ? (existing.started_at ?? now)
      : now
    : existing?.started_at;

  getNavDb()
    .prepare(
      `INSERT INTO nav_orchestrator_state (
        session_id,
        project_id,
        active,
        thread_id,
        started_at,
        updated_at,
        cleared_at
       )
       VALUES (?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(session_id) DO UPDATE SET
        project_id = excluded.project_id,
        active = excluded.active,
        thread_id = excluded.thread_id,
        started_at = excluded.started_at,
        updated_at = excluded.updated_at,
        cleared_at = excluded.cleared_at`,
    )
    .run(
      input.sessionId,
      input.projectId,
      input.active ? 1 : 0,
      input.threadId,
      startedAt ?? null,
      now,
      input.active ? null : now,
    );
};

const insertTurn = (input: {
  difficulty: RequestDifficulty | null;
  error?: string | null;
  isPlanning: boolean;
  mode: OrchestratorMode;
  projectId: string;
  requestText: string;
  sessionId: string;
  status: OrchestratorTurnStatus;
  threadId: string | null;
  turnId: string;
}) => {
  ensureOrchestratorReady();

  const now = Date.now();

  getNavDb()
    .prepare(
      `INSERT INTO nav_orchestrator_turns (
        id,
        session_id,
        project_id,
        thread_id,
        request_text,
        is_planning,
        difficulty,
        mode,
        status,
        error,
        created_at,
        completed_at
       )
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    )
    .run(
      input.turnId,
      input.sessionId,
      input.projectId,
      input.threadId,
      clipText(input.requestText, MAX_REQUEST_TEXT_CHARS),
      input.isPlanning ? 1 : 0,
      input.difficulty,
      input.mode,
      input.status,
      input.error ?? null,
      now,
      input.status === "pending" ? null : now,
    );
};

const updateTurnStatus = (
  turnId: string,
  status: OrchestratorTurnStatus,
  error: string | null = null,
) => {
  getNavDb()
    .prepare(
      `UPDATE nav_orchestrator_turns
       SET status = ?, error = ?, completed_at = ?
       WHERE id = ?`,
    )
    .run(status, error, Date.now(), turnId);
};

const insertDelegateResult = (input: {
  agent: DelegateAgent;
  answer?: string | null;
  completedAt?: number | null;
  error?: string | null;
  sessionId: string;
  startedAt: number;
  status: DelegateStatus;
  turnId: string;
  worktree?: string | null;
}) => {
  getNavDb()
    .prepare(
      `INSERT INTO nav_orchestrator_delegate_results (
        turn_id,
        agent,
        agent_session_id,
        worktree,
        answer,
        status,
        error,
        started_at,
        completed_at
       )
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    )
    .run(
      input.turnId,
      input.agent,
      input.sessionId,
      input.worktree ?? null,
      input.answer ? clipText(input.answer, MAX_DELEGATE_ANSWER_CHARS) : null,
      input.status,
      input.error ?? null,
      input.startedAt,
      input.completedAt ?? null,
    );
};

const toDelegateContext = (
  row: DelegateResultRow,
): OrchestratorDelegateResult | null => {
  if (!isDelegateAgent(row.agent) || !isDelegateStatus(row.status)) {
    return null;
  }

  return {
    agent: row.agent,
    answer: row.answer,
    error: row.error,
    status: row.status,
    worktree: row.worktree,
  };
};

const selectDelegateResults = (turnId: string) =>
  (
    getNavDb()
      .prepare(
        `SELECT
          turn_id,
          agent,
          agent_session_id,
          worktree,
          answer,
          status,
          error,
          started_at,
          completed_at
         FROM nav_orchestrator_delegate_results
         WHERE turn_id = ?
         ORDER BY agent ASC`,
      )
      .all(turnId) as DelegateResultRow[]
  )
    .map(toDelegateContext)
    .filter((row): row is OrchestratorDelegateResult => row !== null);

const rowToTurnContext = (
  row: OrchestratorTurnRow,
  state: OrchestratorStateRow | undefined,
): OrchestratorTurnContext | null => {
  if (!isTurnMode(row.mode) || !isTurnStatus(row.status)) {
    return null;
  }

  return {
    active: state?.active === 1,
    delegateResults: selectDelegateResults(row.id),
    difficulty: isRequestDifficulty(row.difficulty) ? row.difficulty : null,
    error: row.error,
    isPlanning: row.is_planning === 1,
    mode: row.mode,
    status: row.status,
    threadId: row.thread_id,
    turnId: row.id,
  };
};

export const getLatestOrchestratorContext = (
  sessionId: string,
): OrchestratorTurnContext | null => {
  ensureOrchestratorReady();

  const row = getNavDb()
    .prepare(
      `SELECT
        id,
        session_id,
        project_id,
        thread_id,
        request_text,
        is_planning,
        difficulty,
        mode,
        status,
        error,
        created_at,
        completed_at
       FROM nav_orchestrator_turns
       WHERE session_id = ?
       ORDER BY created_at DESC
       LIMIT 1`,
    )
    .get(sessionId) as OrchestratorTurnRow | undefined;

  return row ? rowToTurnContext(row, selectState(sessionId)) : null;
};

export const clearOrchestratorStateForProject = (projectId: string) => {
  ensureOrchestratorReady();

  const now = Date.now();

  getNavDb()
    .prepare(
      `UPDATE nav_orchestrator_state
       SET active = 0, thread_id = NULL, cleared_at = ?, updated_at = ?
       WHERE project_id = ?`,
    )
    .run(now, now, projectId);
};

export const deleteOrchestratorDataForSession = (sessionId: string) => {
  ensureOrchestratorReady();

  const turnRows = getNavDb()
    .prepare("SELECT id FROM nav_orchestrator_turns WHERE session_id = ?")
    .all(sessionId) as { id: string }[];

  for (const row of turnRows) {
    getNavDb()
      .prepare(
        "DELETE FROM nav_orchestrator_delegate_results WHERE turn_id = ?",
      )
      .run(row.id);
  }

  getNavDb()
    .prepare("DELETE FROM nav_orchestrator_turns WHERE session_id = ?")
    .run(sessionId);
  getNavDb()
    .prepare("DELETE FROM nav_orchestrator_state WHERE session_id = ?")
    .run(sessionId);
};

const withTimeout = async <T>(
  run: (signal: AbortSignal) => Promise<T>,
  agent: DelegateAgent,
  signal?: AbortSignal,
) => {
  const timeoutMs = parseDelegateTimeoutMs();
  const controller = new AbortController();
  let timedOut = false;

  const timeout = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, timeoutMs);
  const abortFromSignal = () => controller.abort(signal?.reason);

  if (signal?.aborted) {
    abortFromSignal();
  } else {
    signal?.addEventListener("abort", abortFromSignal, { once: true });
  }

  try {
    return await run(controller.signal);
  } catch (error) {
    if (timedOut) {
      throw new DelegateTimeoutError(agent);
    }

    throw error;
  } finally {
    clearTimeout(timeout);
    signal?.removeEventListener("abort", abortFromSignal);
  }
};

const buildDelegateTask = (input: {
  active: boolean;
  message: string;
  threadId: string;
}) =>
  [
    "Nav is orchestrating this user request and is asking multiple delegate engineers to work independently.",
    "Work only in your own delegate checkout. If implementation is needed, make the change there.",
    "Return a concise summary of your approach, important files changed, and any risks or verification gaps.",
    input.active
      ? "This session is already inside an orchestrated thread. Treat the request as a follow-up unless the user clearly says otherwise."
      : "This is the first delegated turn in this orchestrated thread.",
    `Thread id: ${input.threadId}`,
    `User request:\n${input.message}`,
  ].join("\n\n");

const runDelegate = async (input: {
  agent: DelegateAgent;
  git: GitContext;
  message: string;
  signal?: AbortSignal;
  turnId: string;
}) => {
  const agentSessionId = createDelegationId();
  const startedAt = Date.now();

  try {
    const result = await withTimeout(
      (delegateSignal) =>
        consultAgentWithId(
          input.git,
          input.agent,
          agentSessionId,
          input.message,
          delegateSignal,
        ),
      input.agent,
      input.signal,
    );

    insertDelegateResult({
      agent: input.agent,
      answer: result.answer,
      completedAt: Date.now(),
      sessionId: agentSessionId,
      startedAt,
      status: "complete",
      turnId: input.turnId,
      worktree: result.worktree,
    });

    return { ok: true as const };
  } catch (error) {
    insertDelegateResult({
      agent: input.agent,
      completedAt: Date.now(),
      error: errorMessage(error),
      sessionId: agentSessionId,
      startedAt,
      status: error instanceof DelegateTimeoutError ? "timeout" : "failed",
      turnId: input.turnId,
    });

    return { ok: false as const, error };
  }
};

export const prepareOrchestratorTurn = async (input: {
  git: GitContext;
  hasImages?: boolean;
  message: string;
  project: ResolvedSessionProject;
  sessionId: string;
  signal?: AbortSignal;
}): Promise<OrchestratorTurnContext | null> => {
  if (!input.project.orchestratorEnabled) {
    return null;
  }

  ensureOrchestratorReady();

  const state = selectState(input.sessionId);
  const trimmedMessage = input.message.trim();
  const turnId = createUuidV7();

  if (!trimmedMessage || input.hasImages) {
    const threadId = state?.active === 1 ? state.thread_id : null;

    insertTurn({
      difficulty: null,
      error: input.hasImages
        ? "Image or attachment input is not delegated to text-only delegate agents."
        : "No text was available to classify.",
      isPlanning: false,
      mode: "direct",
      projectId: input.project.id,
      requestText: input.message,
      sessionId: input.sessionId,
      status: "complete",
      threadId,
      turnId,
    });

    return getLatestOrchestratorContext(input.sessionId);
  }

  let classification: {
    difficulty: RequestDifficulty;
    isPlanning: boolean;
  };

  try {
    classification = await requestMessageClassification(
      { text: trimmedMessage },
      input.signal,
    );
  } catch (error) {
    insertTurn({
      difficulty: null,
      error: errorMessage(error),
      isPlanning: false,
      mode: "direct",
      projectId: input.project.id,
      requestText: input.message,
      sessionId: input.sessionId,
      status: "failed",
      threadId: state?.active === 1 ? state.thread_id : null,
      turnId,
    });

    return getLatestOrchestratorContext(input.sessionId);
  }

  const shouldPanel =
    classification.difficulty === "medium" ||
    classification.difficulty === "high";
  const existingThreadId = state?.active === 1 ? state.thread_id : null;
  const threadId = shouldPanel
    ? (existingThreadId ?? createUuidV7())
    : existingThreadId;

  if (shouldPanel) {
    upsertState({
      active: true,
      projectId: input.project.id,
      sessionId: input.sessionId,
      threadId,
    });
  } else if (state?.active === 1) {
    upsertState({
      active: true,
      projectId: input.project.id,
      sessionId: input.sessionId,
      threadId,
    });
  }

  insertTurn({
    difficulty: classification.difficulty,
    isPlanning: classification.isPlanning,
    mode: shouldPanel ? "panel" : "direct",
    projectId: input.project.id,
    requestText: input.message,
    sessionId: input.sessionId,
    status: shouldPanel ? "pending" : "complete",
    threadId,
    turnId,
  });

  if (!shouldPanel || !threadId) {
    return getLatestOrchestratorContext(input.sessionId);
  }

  const delegateTask = buildDelegateTask({
    active: state?.active === 1,
    message: input.message,
    threadId,
  });
  const results = await Promise.all(
    PANEL_AGENTS.map((agent) =>
      runDelegate({
        agent,
        git: input.git,
        message: delegateTask,
        signal: input.signal,
        turnId,
      }),
    ),
  );
  const successCount = results.filter((result) => result.ok).length;

  updateTurnStatus(
    turnId,
    successCount === PANEL_AGENTS.length
      ? "complete"
      : successCount > 0
        ? "partial"
        : "failed",
    successCount === 0 ? "All delegate agents failed." : null,
  );

  return getLatestOrchestratorContext(input.sessionId);
};
