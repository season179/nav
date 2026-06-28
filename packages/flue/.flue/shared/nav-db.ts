import { DatabaseSync } from "node:sqlite";
import type { PersistenceStores } from "@flue/runtime/adapter";
import store from "../db.js";

const DB_PATH = "./data/flue.db";

let db: DatabaseSync | null = null;
let storesPromise: Promise<PersistenceStores> | null = null;

export const getNavDb = () => {
  if (!db) {
    db = new DatabaseSync(DB_PATH);
  }

  return db;
};

export const getNavStores = () => {
  storesPromise ??= Promise.resolve(store.connect());
  return storesPromise;
};

export const migrateNavStore = async () => {
  await Promise.resolve(store.migrate?.());
};

const hasColumn = (tableName: string, columnName: string) =>
  (
    getNavDb().prepare(`PRAGMA table_info(${tableName})`).all() as {
      name: string;
    }[]
  ).some((column) => column.name === columnName);

export const ensureNavSessionTable = () => {
  const sql = getNavDb();

  sql.exec(`
    CREATE TABLE IF NOT EXISTS nav_sessions (
      id TEXT PRIMARY KEY,
      agent_name TEXT NOT NULL DEFAULT 'nav',
      title TEXT,
      title_source TEXT NOT NULL DEFAULT 'first-message',
      pinned INTEGER NOT NULL DEFAULT 0,
      archived INTEGER NOT NULL DEFAULT 0,
      project_id TEXT,
      created_at INTEGER NOT NULL,
      last_opened_at INTEGER,
      imported_at INTEGER
    )
  `);

  if (!hasColumn("nav_sessions", "project_id")) {
    sql.exec("ALTER TABLE nav_sessions ADD COLUMN project_id TEXT");
  }

  sql.exec(`
    CREATE INDEX IF NOT EXISTS nav_sessions_agent_archived_idx
      ON nav_sessions (agent_name, archived, pinned)
  `);
  sql.exec(`
    CREATE INDEX IF NOT EXISTS nav_sessions_project_idx
      ON nav_sessions (project_id)
  `);
};

export const ensureMessageClassificationsReady = () => {
  const sql = getNavDb();

  sql.exec(`
    CREATE TABLE IF NOT EXISTS nav_message_classifications (
      session_id TEXT NOT NULL,
      message_id TEXT NOT NULL,
      is_planning INTEGER NOT NULL,
      difficulty TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      PRIMARY KEY (session_id, message_id)
    )
  `);
};

export const ensureNavProjectTable = () => {
  const sql = getNavDb();

  sql.exec(`
    CREATE TABLE IF NOT EXISTS nav_projects (
      id TEXT PRIMARY KEY,
      name TEXT NOT NULL,
      path TEXT NOT NULL,
      display_path TEXT,
      is_default INTEGER NOT NULL DEFAULT 0,
      archived INTEGER NOT NULL DEFAULT 0,
      model_spec TEXT,
      auto_approve_edits INTEGER NOT NULL DEFAULT 0,
      orchestrator_enabled INTEGER NOT NULL DEFAULT 0,
      color TEXT,
      icon TEXT,
      sort_order INTEGER,
      created_at INTEGER NOT NULL,
      last_opened_at INTEGER
    )
  `);

  for (const [column, definition] of [
    ["model_spec", "TEXT"],
    ["auto_approve_edits", "INTEGER NOT NULL DEFAULT 0"],
    ["orchestrator_enabled", "INTEGER NOT NULL DEFAULT 0"],
    ["color", "TEXT"],
    ["icon", "TEXT"],
    ["sort_order", "INTEGER"],
  ] as const) {
    if (!hasColumn("nav_projects", column)) {
      sql.exec(`ALTER TABLE nav_projects ADD COLUMN ${column} ${definition}`);
    }
  }

  sql.exec(`
    CREATE UNIQUE INDEX IF NOT EXISTS nav_projects_path_unique_idx
      ON nav_projects (path COLLATE NOCASE)
  `);
  sql.exec(`
    CREATE INDEX IF NOT EXISTS nav_projects_archived_opened_idx
      ON nav_projects (archived, last_opened_at, created_at)
  `);
  sql.exec(`
    CREATE INDEX IF NOT EXISTS nav_projects_default_idx
      ON nav_projects (is_default)
  `);
};

export const ensureOrchestratorReady = () => {
  const sql = getNavDb();

  sql.exec(`
    CREATE TABLE IF NOT EXISTS nav_orchestrator_state (
      session_id TEXT PRIMARY KEY,
      project_id TEXT,
      active INTEGER NOT NULL DEFAULT 0,
      thread_id TEXT,
      started_at INTEGER,
      updated_at INTEGER NOT NULL,
      cleared_at INTEGER
    )
  `);
  sql.exec(`
    CREATE TABLE IF NOT EXISTS nav_orchestrator_turns (
      id TEXT PRIMARY KEY,
      session_id TEXT NOT NULL,
      project_id TEXT,
      thread_id TEXT,
      request_text TEXT NOT NULL,
      is_planning INTEGER NOT NULL,
      difficulty TEXT,
      mode TEXT NOT NULL,
      status TEXT NOT NULL,
      error TEXT,
      created_at INTEGER NOT NULL,
      completed_at INTEGER
    )
  `);
  sql.exec(`
    CREATE INDEX IF NOT EXISTS nav_orchestrator_turns_session_idx
      ON nav_orchestrator_turns (session_id, created_at)
  `);
  sql.exec(`
    CREATE TABLE IF NOT EXISTS nav_orchestrator_delegate_results (
      turn_id TEXT NOT NULL,
      agent TEXT NOT NULL,
      agent_session_id TEXT NOT NULL,
      worktree TEXT,
      answer TEXT,
      status TEXT NOT NULL,
      error TEXT,
      started_at INTEGER NOT NULL,
      completed_at INTEGER,
      PRIMARY KEY (turn_id, agent)
    )
  `);
  sql.exec(`
    CREATE INDEX IF NOT EXISTS nav_orchestrator_delegate_results_turn_idx
      ON nav_orchestrator_delegate_results (turn_id)
  `);
};
