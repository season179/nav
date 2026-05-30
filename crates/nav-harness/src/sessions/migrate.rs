//! SQLite schema migrations for canonical session storage.

use std::collections::HashMap;

use rusqlite::{Connection, params};

pub const SCHEMA_VERSION: i64 = 1;

pub const CORE_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version     INTEGER PRIMARY KEY NOT NULL,
    applied_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY NOT NULL,
    title           TEXT,
    source          TEXT NOT NULL DEFAULT 'cli',
    workspace_root  TEXT,
    system_prompt   TEXT,
    settings_json   TEXT NOT NULL DEFAULT '{}',
    parent_id       TEXT REFERENCES sessions(id),
    version         TEXT NOT NULL,
    slug            TEXT,
    cost            REAL NOT NULL DEFAULT 0,
    tokens_input    INTEGER NOT NULL DEFAULT 0,
    tokens_output   INTEGER NOT NULL DEFAULT 0,
    tokens_reasoning INTEGER NOT NULL DEFAULT 0,
    tokens_cache_read  INTEGER NOT NULL DEFAULT 0,
    tokens_cache_write INTEGER NOT NULL DEFAULT 0,
    time_archived   INTEGER,
    time_compacting INTEGER,
    revert_json     TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
    id              TEXT PRIMARY KEY NOT NULL,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    status          TEXT NOT NULL,
    trigger         TEXT,
    started_at      INTEGER NOT NULL,
    finished_at     INTEGER,
    error_json      TEXT
);

CREATE INDEX IF NOT EXISTS idx_runs_session_started ON runs(session_id, started_at DESC);

CREATE TABLE IF NOT EXISTS turns (
    id              TEXT PRIMARY KEY NOT NULL,
    run_id          TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    role            TEXT NOT NULL,
    model_id        TEXT,
    meta_json       TEXT NOT NULL DEFAULT '{}',
    created_at      INTEGER NOT NULL,
    UNIQUE (run_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_turns_run_seq ON turns(run_id, seq);

CREATE TABLE IF NOT EXISTS turn_parts (
    id              TEXT PRIMARY KEY NOT NULL,
    turn_id         TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    type            TEXT NOT NULL,
    data_json       TEXT NOT NULL,
    provider_payload_id TEXT REFERENCES provider_payloads(id) ON DELETE SET NULL,
    provider_json_pointer TEXT,
    compacted_at    INTEGER,
    created_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_turn_parts_turn_id ON turn_parts(turn_id, id);
CREATE INDEX IF NOT EXISTS idx_turn_parts_session_id ON turn_parts(session_id);

CREATE TABLE IF NOT EXISTS artifacts (
    id              TEXT PRIMARY KEY NOT NULL,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    part_id         TEXT REFERENCES turn_parts(id) ON DELETE SET NULL,
    kind            TEXT NOT NULL,
    mime            TEXT NOT NULL,
    sha256          TEXT NOT NULL,
    path            TEXT NOT NULL,
    size_bytes      INTEGER NOT NULL,
    created_at      INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_artifacts_sha256 ON artifacts(sha256);

CREATE TABLE IF NOT EXISTS provider_payloads (
    id                  TEXT PRIMARY KEY NOT NULL,
    session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id              TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    direction           TEXT NOT NULL,
    api_kind            TEXT NOT NULL,
    provider_id         TEXT,
    model_id            TEXT,
    sequence            INTEGER NOT NULL,
    provider_payload_id TEXT,
    artifact_id         TEXT NOT NULL REFERENCES artifacts(id),
    sha256              TEXT NOT NULL,
    decoder_version     TEXT,
    decode_status       TEXT NOT NULL DEFAULT 'pending',
    error_json          TEXT,
    created_at          INTEGER NOT NULL,
    decoded_at          INTEGER,
    UNIQUE (run_id, direction, sequence)
);

CREATE INDEX IF NOT EXISTS idx_provider_payloads_run_sequence ON provider_payloads(run_id, sequence);
CREATE INDEX IF NOT EXISTS idx_provider_payloads_session ON provider_payloads(session_id, created_at);

CREATE TABLE IF NOT EXISTS provider_state (
    run_id          TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    api_kind        TEXT NOT NULL,
    state_json      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS turn_parts_text (
    part_id     TEXT PRIMARY KEY NOT NULL REFERENCES turn_parts(id) ON DELETE CASCADE,
    turn_id     TEXT NOT NULL,
    part_type   TEXT NOT NULL,
    text        TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_turn_parts_text_turn_id ON turn_parts_text(turn_id);

-- Projection rules for turn_parts_text (FTS-01a):
--   • 'text'        → extracts $.text        (user/assistant text content)
--   • 'tool_result' → extracts $.content      (tool output for search)
--   • 'thinking'    → extracts $.text          (model reasoning traces)
--   • All other types (tool_call, image, step_start, step_finish,
--     compaction, retry, snapshot, provider_opaque) are EXCLUDED.
--     tool_call.arguments is JSON structure, not displayable text;
--     image is binary; step/compaction/retry/snapshot/opaque are metadata.
--
-- Triggers keep this table in lockstep with turn_parts writes:
--   INSERT  → project if type is included and text is non-empty
--   UPDATE  → unconditionally delete old projection, then re-project if type is included
--   DELETE  → cascade via FK + explicit trigger cleanup

CREATE TRIGGER IF NOT EXISTS trg_turn_parts_text_insert
AFTER INSERT ON turn_parts
BEGIN
    INSERT OR REPLACE INTO turn_parts_text (part_id, turn_id, part_type, text)
    SELECT NEW.id, NEW.turn_id, NEW.type,
        CASE NEW.type
            WHEN 'text' THEN json_extract(NEW.data_json, '$.text')
            WHEN 'tool_result' THEN json_extract(NEW.data_json, '$.content')
            WHEN 'thinking' THEN json_extract(NEW.data_json, '$.text')
        END
    WHERE NEW.type IN ('text', 'tool_result', 'thinking')
      AND COALESCE(
          CASE NEW.type
              WHEN 'text' THEN json_extract(NEW.data_json, '$.text')
              WHEN 'tool_result' THEN json_extract(NEW.data_json, '$.content')
              WHEN 'thinking' THEN json_extract(NEW.data_json, '$.text')
          END,
          ''
      ) != '';
END;

CREATE TRIGGER IF NOT EXISTS trg_turn_parts_text_update
AFTER UPDATE ON turn_parts
BEGIN
    DELETE FROM turn_parts_text WHERE part_id = NEW.id;
    INSERT OR REPLACE INTO turn_parts_text (part_id, turn_id, part_type, text)
    SELECT NEW.id, NEW.turn_id, NEW.type,
        CASE NEW.type
            WHEN 'text' THEN json_extract(NEW.data_json, '$.text')
            WHEN 'tool_result' THEN json_extract(NEW.data_json, '$.content')
            WHEN 'thinking' THEN json_extract(NEW.data_json, '$.text')
        END
    WHERE NEW.type IN ('text', 'tool_result', 'thinking')
      AND COALESCE(
          CASE NEW.type
              WHEN 'text' THEN json_extract(NEW.data_json, '$.text')
              WHEN 'tool_result' THEN json_extract(NEW.data_json, '$.content')
              WHEN 'thinking' THEN json_extract(NEW.data_json, '$.text')
          END,
          ''
      ) != '';
END;

CREATE TRIGGER IF NOT EXISTS trg_turn_parts_text_delete
AFTER DELETE ON turn_parts
BEGIN
    DELETE FROM turn_parts_text WHERE part_id = OLD.id;
END;

CREATE VIRTUAL TABLE IF NOT EXISTS turn_parts_fts USING fts5(
    part_id UNINDEXED,
    turn_id UNINDEXED,
    text,
    tokenize='unicode61'
);
"#;

const CREATE_TRIGRAM_FTS_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS turn_parts_fts_trigram USING fts5(
    part_id UNINDEXED,
    turn_id UNINDEXED,
    text,
    tokenize='trigram'
);
"#;

const CREATE_TRIGRAM_FTS_FALLBACK_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS turn_parts_fts_trigram USING fts5(
    part_id UNINDEXED,
    turn_id UNINDEXED,
    text
);
"#;

const FTS_SYNC_SQL: &str = r#"
INSERT INTO turn_parts_fts (rowid, part_id, turn_id, text)
SELECT tpt.rowid, tpt.part_id, tpt.turn_id, tpt.text
FROM turn_parts_text tpt
LEFT JOIN turn_parts_fts fts ON fts.rowid = tpt.rowid
WHERE fts.rowid IS NULL;

INSERT INTO turn_parts_fts_trigram (rowid, part_id, turn_id, text)
SELECT tpt.rowid, tpt.part_id, tpt.turn_id, tpt.text
FROM turn_parts_text tpt
LEFT JOIN turn_parts_fts_trigram fts ON fts.rowid = tpt.rowid
WHERE fts.rowid IS NULL;

CREATE TRIGGER IF NOT EXISTS trg_turn_parts_fts_insert
AFTER INSERT ON turn_parts_text
BEGIN
    INSERT INTO turn_parts_fts (rowid, part_id, turn_id, text)
    VALUES (NEW.rowid, NEW.part_id, NEW.turn_id, NEW.text);
    INSERT INTO turn_parts_fts_trigram (rowid, part_id, turn_id, text)
    VALUES (NEW.rowid, NEW.part_id, NEW.turn_id, NEW.text);
END;

CREATE TRIGGER IF NOT EXISTS trg_turn_parts_fts_update
AFTER UPDATE ON turn_parts_text
BEGIN
    DELETE FROM turn_parts_fts WHERE rowid = OLD.rowid;
    DELETE FROM turn_parts_fts_trigram WHERE rowid = OLD.rowid;
    INSERT INTO turn_parts_fts (rowid, part_id, turn_id, text)
    VALUES (NEW.rowid, NEW.part_id, NEW.turn_id, NEW.text);
    INSERT INTO turn_parts_fts_trigram (rowid, part_id, turn_id, text)
    VALUES (NEW.rowid, NEW.part_id, NEW.turn_id, NEW.text);
END;

CREATE TRIGGER IF NOT EXISTS trg_turn_parts_fts_delete
AFTER DELETE ON turn_parts_text
BEGIN
    DELETE FROM turn_parts_fts WHERE rowid = OLD.rowid;
    DELETE FROM turn_parts_fts_trigram WHERE rowid = OLD.rowid;
END;
"#;

struct TableSchema {
    name: &'static str,
    columns: &'static [ColumnSchema],
}

struct ColumnSchema {
    name: &'static str,
    sql: &'static str,
    data_type: &'static str,
    not_null: bool,
    default_sql: Option<&'static str>,
    primary_key: bool,
}

const TABLES: &[TableSchema] = &[
    TableSchema {
        name: "schema_migrations",
        columns: &[
            column(
                "version",
                "version INTEGER PRIMARY KEY NOT NULL",
                "INTEGER",
                true,
                None,
                true,
            ),
            column(
                "applied_at",
                "applied_at INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
        ],
    },
    TableSchema {
        name: "sessions",
        columns: &[
            column(
                "id",
                "id TEXT PRIMARY KEY NOT NULL",
                "TEXT",
                true,
                None,
                true,
            ),
            column("title", "title TEXT", "TEXT", false, None, false),
            column(
                "source",
                "source TEXT NOT NULL DEFAULT 'cli'",
                "TEXT",
                true,
                Some("'cli'"),
                false,
            ),
            column(
                "workspace_root",
                "workspace_root TEXT",
                "TEXT",
                false,
                None,
                false,
            ),
            column(
                "system_prompt",
                "system_prompt TEXT",
                "TEXT",
                false,
                None,
                false,
            ),
            column(
                "settings_json",
                "settings_json TEXT NOT NULL DEFAULT '{}'",
                "TEXT",
                true,
                Some("'{}'"),
                false,
            ),
            column(
                "parent_id",
                "parent_id TEXT REFERENCES sessions(id)",
                "TEXT",
                false,
                None,
                false,
            ),
            column(
                "version",
                "version TEXT NOT NULL",
                "TEXT",
                true,
                None,
                false,
            ),
            column("slug", "slug TEXT", "TEXT", false, None, false),
            column(
                "cost",
                "cost REAL NOT NULL DEFAULT 0",
                "REAL",
                true,
                Some("0"),
                false,
            ),
            column(
                "tokens_input",
                "tokens_input INTEGER NOT NULL DEFAULT 0",
                "INTEGER",
                true,
                Some("0"),
                false,
            ),
            column(
                "tokens_output",
                "tokens_output INTEGER NOT NULL DEFAULT 0",
                "INTEGER",
                true,
                Some("0"),
                false,
            ),
            column(
                "tokens_reasoning",
                "tokens_reasoning INTEGER NOT NULL DEFAULT 0",
                "INTEGER",
                true,
                Some("0"),
                false,
            ),
            column(
                "tokens_cache_read",
                "tokens_cache_read INTEGER NOT NULL DEFAULT 0",
                "INTEGER",
                true,
                Some("0"),
                false,
            ),
            column(
                "tokens_cache_write",
                "tokens_cache_write INTEGER NOT NULL DEFAULT 0",
                "INTEGER",
                true,
                Some("0"),
                false,
            ),
            column(
                "time_archived",
                "time_archived INTEGER",
                "INTEGER",
                false,
                None,
                false,
            ),
            column(
                "time_compacting",
                "time_compacting INTEGER",
                "INTEGER",
                false,
                None,
                false,
            ),
            column(
                "revert_json",
                "revert_json TEXT",
                "TEXT",
                false,
                None,
                false,
            ),
            column(
                "created_at",
                "created_at INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
            column(
                "updated_at",
                "updated_at INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
        ],
    },
    TableSchema {
        name: "runs",
        columns: &[
            column(
                "id",
                "id TEXT PRIMARY KEY NOT NULL",
                "TEXT",
                true,
                None,
                true,
            ),
            column(
                "session_id",
                "session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE",
                "TEXT",
                true,
                None,
                false,
            ),
            column("status", "status TEXT NOT NULL", "TEXT", true, None, false),
            column("trigger", "trigger TEXT", "TEXT", false, None, false),
            column(
                "started_at",
                "started_at INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
            column(
                "finished_at",
                "finished_at INTEGER",
                "INTEGER",
                false,
                None,
                false,
            ),
            column("error_json", "error_json TEXT", "TEXT", false, None, false),
        ],
    },
    TableSchema {
        name: "turns",
        columns: &[
            column(
                "id",
                "id TEXT PRIMARY KEY NOT NULL",
                "TEXT",
                true,
                None,
                true,
            ),
            column(
                "run_id",
                "run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE",
                "TEXT",
                true,
                None,
                false,
            ),
            column("seq", "seq INTEGER NOT NULL", "INTEGER", true, None, false),
            column("role", "role TEXT NOT NULL", "TEXT", true, None, false),
            column("model_id", "model_id TEXT", "TEXT", false, None, false),
            column(
                "meta_json",
                "meta_json TEXT NOT NULL DEFAULT '{}'",
                "TEXT",
                true,
                Some("'{}'"),
                false,
            ),
            column(
                "created_at",
                "created_at INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
        ],
    },
    TableSchema {
        name: "turn_parts",
        columns: &[
            column(
                "id",
                "id TEXT PRIMARY KEY NOT NULL",
                "TEXT",
                true,
                None,
                true,
            ),
            column(
                "turn_id",
                "turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE",
                "TEXT",
                true,
                None,
                false,
            ),
            column(
                "session_id",
                "session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE",
                "TEXT",
                true,
                None,
                false,
            ),
            column("type", "type TEXT NOT NULL", "TEXT", true, None, false),
            column(
                "data_json",
                "data_json TEXT NOT NULL",
                "TEXT",
                true,
                None,
                false,
            ),
            column(
                "provider_payload_id",
                "provider_payload_id TEXT REFERENCES provider_payloads(id) ON DELETE SET NULL",
                "TEXT",
                false,
                None,
                false,
            ),
            column(
                "provider_json_pointer",
                "provider_json_pointer TEXT",
                "TEXT",
                false,
                None,
                false,
            ),
            column(
                "compacted_at",
                "compacted_at INTEGER",
                "INTEGER",
                false,
                None,
                false,
            ),
            column(
                "created_at",
                "created_at INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
        ],
    },
    TableSchema {
        name: "artifacts",
        columns: &[
            column(
                "id",
                "id TEXT PRIMARY KEY NOT NULL",
                "TEXT",
                true,
                None,
                true,
            ),
            column(
                "session_id",
                "session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE",
                "TEXT",
                true,
                None,
                false,
            ),
            column(
                "part_id",
                "part_id TEXT REFERENCES turn_parts(id) ON DELETE SET NULL",
                "TEXT",
                false,
                None,
                false,
            ),
            column("kind", "kind TEXT NOT NULL", "TEXT", true, None, false),
            column("mime", "mime TEXT NOT NULL", "TEXT", true, None, false),
            column("sha256", "sha256 TEXT NOT NULL", "TEXT", true, None, false),
            column("path", "path TEXT NOT NULL", "TEXT", true, None, false),
            column(
                "size_bytes",
                "size_bytes INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
            column(
                "created_at",
                "created_at INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
        ],
    },
    TableSchema {
        name: "provider_payloads",
        columns: &[
            column(
                "id",
                "id TEXT PRIMARY KEY NOT NULL",
                "TEXT",
                true,
                None,
                true,
            ),
            column(
                "session_id",
                "session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE",
                "TEXT",
                true,
                None,
                false,
            ),
            column(
                "run_id",
                "run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE",
                "TEXT",
                true,
                None,
                false,
            ),
            column(
                "direction",
                "direction TEXT NOT NULL",
                "TEXT",
                true,
                None,
                false,
            ),
            column(
                "api_kind",
                "api_kind TEXT NOT NULL",
                "TEXT",
                true,
                None,
                false,
            ),
            column(
                "provider_id",
                "provider_id TEXT",
                "TEXT",
                false,
                None,
                false,
            ),
            column("model_id", "model_id TEXT", "TEXT", false, None, false),
            column(
                "sequence",
                "sequence INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
            column(
                "provider_payload_id",
                "provider_payload_id TEXT",
                "TEXT",
                false,
                None,
                false,
            ),
            column(
                "artifact_id",
                "artifact_id TEXT NOT NULL REFERENCES artifacts(id)",
                "TEXT",
                true,
                None,
                false,
            ),
            column("sha256", "sha256 TEXT NOT NULL", "TEXT", true, None, false),
            column(
                "decoder_version",
                "decoder_version TEXT",
                "TEXT",
                false,
                None,
                false,
            ),
            column(
                "decode_status",
                "decode_status TEXT NOT NULL DEFAULT 'pending'",
                "TEXT",
                true,
                Some("'pending'"),
                false,
            ),
            column("error_json", "error_json TEXT", "TEXT", false, None, false),
            column(
                "created_at",
                "created_at INTEGER NOT NULL",
                "INTEGER",
                true,
                None,
                false,
            ),
            column(
                "decoded_at",
                "decoded_at INTEGER",
                "INTEGER",
                false,
                None,
                false,
            ),
        ],
    },
    TableSchema {
        name: "provider_state",
        columns: &[
            column(
                "run_id",
                "run_id TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE",
                "TEXT",
                true,
                None,
                true,
            ),
            column(
                "api_kind",
                "api_kind TEXT NOT NULL",
                "TEXT",
                true,
                None,
                false,
            ),
            column(
                "state_json",
                "state_json TEXT NOT NULL",
                "TEXT",
                true,
                None,
                false,
            ),
        ],
    },
    TableSchema {
        name: "turn_parts_text",
        columns: &[
            column(
                "part_id",
                "part_id TEXT PRIMARY KEY NOT NULL REFERENCES turn_parts(id) ON DELETE CASCADE",
                "TEXT",
                true,
                None,
                true,
            ),
            column(
                "turn_id",
                "turn_id TEXT NOT NULL",
                "TEXT",
                true,
                None,
                false,
            ),
            column(
                "part_type",
                "part_type TEXT NOT NULL",
                "TEXT",
                true,
                None,
                false,
            ),
            column("text", "text TEXT NOT NULL", "TEXT", true, None, false),
        ],
    },
];

const INDEXES: &[IndexSchema] = &[
    IndexSchema {
        name: "idx_runs_session_started",
        table: "runs",
        sql: "CREATE INDEX idx_runs_session_started ON runs(session_id, started_at DESC)",
    },
    IndexSchema {
        name: "idx_turns_run_seq",
        table: "turns",
        sql: "CREATE INDEX idx_turns_run_seq ON turns(run_id, seq)",
    },
    IndexSchema {
        name: "idx_turn_parts_turn_id",
        table: "turn_parts",
        sql: "CREATE INDEX idx_turn_parts_turn_id ON turn_parts(turn_id, id)",
    },
    IndexSchema {
        name: "idx_turn_parts_session_id",
        table: "turn_parts",
        sql: "CREATE INDEX idx_turn_parts_session_id ON turn_parts(session_id)",
    },
    IndexSchema {
        name: "idx_artifacts_sha256",
        table: "artifacts",
        sql: "CREATE UNIQUE INDEX idx_artifacts_sha256 ON artifacts(sha256)",
    },
    IndexSchema {
        name: "idx_provider_payloads_run_sequence",
        table: "provider_payloads",
        sql: "CREATE INDEX idx_provider_payloads_run_sequence ON provider_payloads(run_id, sequence)",
    },
    IndexSchema {
        name: "idx_provider_payloads_session",
        table: "provider_payloads",
        sql: "CREATE INDEX idx_provider_payloads_session ON provider_payloads(session_id, created_at)",
    },
    IndexSchema {
        name: "idx_turn_parts_text_turn_id",
        table: "turn_parts_text",
        sql: "CREATE INDEX idx_turn_parts_text_turn_id ON turn_parts_text(turn_id)",
    },
];

struct IndexSchema {
    name: &'static str,
    table: &'static str,
    sql: &'static str,
}

const fn column(
    name: &'static str,
    sql: &'static str,
    data_type: &'static str,
    not_null: bool,
    default_sql: Option<&'static str>,
    primary_key: bool,
) -> ColumnSchema {
    ColumnSchema {
        name,
        sql,
        data_type,
        not_null,
        default_sql,
        primary_key,
    }
}

pub fn migrate(conn: &Connection) -> Result<(), MigrationError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = migrate_inner(conn);

    if let Err(err) = result {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(err);
    }

    conn.execute_batch("COMMIT")?;
    Ok(())
}

fn migrate_inner(conn: &Connection) -> Result<(), MigrationError> {
    conn.execute_batch(CORE_SCHEMA_SQL)?;
    create_trigram_fts_table(conn)?;
    conn.execute_batch(FTS_SYNC_SQL)?;
    reconcile_declared_schema(conn)?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
        params![SCHEMA_VERSION, unix_millis()],
    )?;
    Ok(())
}

fn create_trigram_fts_table(conn: &Connection) -> Result<(), MigrationError> {
    create_trigram_fts_table_with_sql(conn, CREATE_TRIGRAM_FTS_SQL)
}

fn create_trigram_fts_table_with_sql(
    conn: &Connection,
    trigram_sql: &str,
) -> Result<(), MigrationError> {
    match conn.execute_batch(trigram_sql) {
        Ok(()) => Ok(()),
        Err(err) => {
            warn_trigram_fallback_once(&err);
            conn.execute_batch(CREATE_TRIGRAM_FTS_FALLBACK_SQL)?;
            Ok(())
        }
    }
}

fn warn_trigram_fallback_once(err: &rusqlite::Error) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let message = err.to_string();
    ONCE.call_once(|| {
        eprintln!(
            "nav: SQLite FTS5 trigram tokenizer unavailable ({message}); \
             falling back to default FTS tokenizer for turn_parts_fts_trigram"
        );
    });
}

fn reconcile_declared_schema(conn: &Connection) -> Result<(), MigrationError> {
    for table in TABLES {
        let actual_columns = read_table_columns(conn, table.name)?;
        for expected in table.columns {
            match actual_columns.get(expected.name) {
                Some(actual) => actual.check_compatible(table.name, expected)?,
                None => add_missing_column(conn, table.name, expected)?,
            }
        }
    }

    for index in INDEXES {
        check_index(conn, index)?;
    }

    Ok(())
}

fn check_index(conn: &Connection, expected: &IndexSchema) -> Result<(), MigrationError> {
    let (table, sql): (String, String) = conn
        .query_row(
            "SELECT tbl_name, sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
            [expected.name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => MigrationError::MissingIndex {
                name: expected.name,
            },
            other => MigrationError::Sqlite(other),
        })?;

    if table != expected.table || sql != expected.sql {
        return Err(MigrationError::IncompatibleIndex {
            name: expected.name,
            expected: expected.sql.to_string(),
            actual: sql,
        });
    }

    Ok(())
}

fn read_table_columns(
    conn: &Connection,
    table: &'static str,
) -> Result<HashMap<String, ActualColumn>, MigrationError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| {
        Ok(ActualColumn {
            name: row.get(1)?,
            data_type: row.get(2)?,
            not_null: row.get::<_, i64>(3)? != 0,
            default_sql: row.get(4)?,
            primary_key: row.get::<_, i64>(5)? != 0,
        })
    })?;

    let mut by_name = HashMap::new();
    for column in columns {
        let column = column?;
        by_name.insert(column.name.clone(), column);
    }
    Ok(by_name)
}

fn add_missing_column(
    conn: &Connection,
    table: &'static str,
    column: &ColumnSchema,
) -> Result<(), MigrationError> {
    if column.primary_key || column.not_null {
        return Err(MigrationError::MissingRequiredColumn {
            table,
            column: column.name,
        });
    }

    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {}", column.sql),
        [],
    )?;
    Ok(())
}

#[derive(Debug)]
struct ActualColumn {
    name: String,
    data_type: String,
    not_null: bool,
    default_sql: Option<String>,
    primary_key: bool,
}

impl ActualColumn {
    fn check_compatible(
        &self,
        table: &'static str,
        expected: &ColumnSchema,
    ) -> Result<(), MigrationError> {
        let actual_type = self.data_type.trim().to_ascii_uppercase();
        if actual_type != expected.data_type {
            return Err(MigrationError::IncompatibleColumn {
                table,
                column: expected.name,
                expected: expected.data_type,
                actual: self.data_type.clone(),
            });
        }

        if self.not_null != expected.not_null && !self.primary_key {
            return Err(MigrationError::IncompatibleColumn {
                table,
                column: expected.name,
                expected: if expected.not_null {
                    "NOT NULL"
                } else {
                    "NULL"
                },
                actual: if self.not_null { "NOT NULL" } else { "NULL" }.to_string(),
            });
        }

        if default_sql(self.default_sql.as_deref()) != default_sql(expected.default_sql) {
            return Err(MigrationError::IncompatibleColumn {
                table,
                column: expected.name,
                expected: expected.default_sql.unwrap_or("no default"),
                actual: self
                    .default_sql
                    .clone()
                    .unwrap_or_else(|| "no default".to_string()),
            });
        }

        if self.primary_key != expected.primary_key {
            return Err(MigrationError::IncompatibleColumn {
                table,
                column: expected.name,
                expected: if expected.primary_key {
                    "PRIMARY KEY"
                } else {
                    "not primary key"
                },
                actual: if self.primary_key {
                    "PRIMARY KEY"
                } else {
                    "not primary key"
                }
                .to_string(),
            });
        }

        Ok(())
    }
}

fn default_sql(value: Option<&str>) -> Option<String> {
    value.map(|sql| sql.trim().to_string())
}

#[derive(Debug)]
pub enum MigrationError {
    Sqlite(rusqlite::Error),
    MissingRequiredColumn {
        table: &'static str,
        column: &'static str,
    },
    MissingIndex {
        name: &'static str,
    },
    IncompatibleColumn {
        table: &'static str,
        column: &'static str,
        expected: &'static str,
        actual: String,
    },
    IncompatibleIndex {
        name: &'static str,
        expected: String,
        actual: String,
    },
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(err) => write!(f, "{err}"),
            Self::MissingRequiredColumn { table, column } => {
                write!(f, "missing required column {table}.{column}")
            }
            Self::MissingIndex { name } => write!(f, "missing index {name}"),
            Self::IncompatibleColumn {
                table,
                column,
                expected,
                actual,
            } => write!(
                f,
                "incompatible column {table}.{column}: expected {expected}, got {actual}"
            ),
            Self::IncompatibleIndex {
                name,
                expected,
                actual,
            } => write!(
                f,
                "incompatible index {name}: expected {expected}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<rusqlite::Error> for MigrationError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigram_fts_creation_falls_back_when_tokenizer_is_unavailable() {
        let conn = Connection::open_in_memory().expect("in-memory connection should open");
        create_trigram_fts_table_with_sql(
            &conn,
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS turn_parts_fts_trigram USING fts5(
                part_id UNINDEXED,
                turn_id UNINDEXED,
                text,
                tokenize='nav_missing_tokenizer'
            );
            "#,
        )
        .expect("fallback FTS table should be created");

        let table_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'turn_parts_fts_trigram'",
                [],
                |row| row.get(0),
            )
            .expect("fallback FTS table should be present");
        assert!(table_sql.contains("turn_parts_fts_trigram"));

        conn.execute(
            "INSERT INTO turn_parts_fts_trigram (rowid, part_id, turn_id, text)
             VALUES (1, 'part', 'turn', 'fallback search text')",
            [],
        )
        .expect("fallback FTS table should accept rows");
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM turn_parts_fts_trigram
                 WHERE turn_parts_fts_trigram MATCH 'fallback'",
                [],
                |row| row.get(0),
            )
            .expect("fallback FTS table should be searchable");

        assert_eq!(hits, 1);
    }

    #[test]
    fn migrate_adds_turns_model_id_to_legacy_schema() {
        let conn = Connection::open_in_memory().expect("in-memory connection should open");
        // A pre-#466 `turns` table that predates the model_id column.
        conn.execute_batch(
            r#"
            CREATE TABLE turns (
                id              TEXT PRIMARY KEY NOT NULL,
                run_id          TEXT NOT NULL,
                seq             INTEGER NOT NULL,
                role            TEXT NOT NULL,
                meta_json       TEXT NOT NULL DEFAULT '{}',
                created_at      INTEGER NOT NULL,
                UNIQUE (run_id, seq)
            );
            "#,
        )
        .expect("legacy turns table should be created");

        migrate(&conn).expect("migration should reconcile the legacy schema");

        let columns = read_table_columns(&conn, "turns").expect("turns columns should be readable");
        assert!(
            columns.contains_key("model_id"),
            "reconciliation should add the turns.model_id column without a manual migration"
        );
    }
}
