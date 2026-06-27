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
      created_at INTEGER NOT NULL,
      last_opened_at INTEGER
    )
  `);
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
