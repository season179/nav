-- Canonical nav session-storage schema (migration version 1).
-- Captured verbatim from the existing ~/.nav/nav.db; do NOT modify the
-- structure — it is shared with the pi CLI and stores raw provider
-- payloads (OpenAI Responses / Anthropic) alongside conversation turns.
-- Applied by nav only when opening a database that has no tables yet.

CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY NOT NULL,
    applied_at  INTEGER NOT NULL
);

CREATE TABLE sessions (
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

CREATE TABLE runs (
    id              TEXT PRIMARY KEY NOT NULL,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    status          TEXT NOT NULL,
    trigger         TEXT,
    started_at      INTEGER NOT NULL,
    finished_at     INTEGER,
    error_json      TEXT
);

CREATE TABLE turns (
    id              TEXT PRIMARY KEY NOT NULL,
    run_id          TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    role            TEXT NOT NULL,
    meta_json       TEXT NOT NULL DEFAULT '{}',
    created_at      INTEGER NOT NULL, model_id TEXT,
    UNIQUE (run_id, seq)
);

CREATE TABLE turn_parts (
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

CREATE TABLE artifacts (
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

CREATE TABLE provider_payloads (
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

CREATE TABLE provider_state (
    run_id          TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    api_kind        TEXT NOT NULL,
    state_json      TEXT NOT NULL
);

CREATE TABLE turn_parts_text (
    part_id     TEXT PRIMARY KEY NOT NULL REFERENCES turn_parts(id) ON DELETE CASCADE,
    turn_id     TEXT NOT NULL,
    part_type   TEXT NOT NULL,
    text        TEXT NOT NULL
);

CREATE VIRTUAL TABLE turn_parts_fts USING fts5(
    part_id UNINDEXED,
    turn_id UNINDEXED,
    text,
    tokenize='unicode61'
);

CREATE VIRTUAL TABLE turn_parts_fts_trigram USING fts5(
    part_id UNINDEXED,
    turn_id UNINDEXED,
    text,
    tokenize='trigram'
);

CREATE INDEX idx_runs_session_started ON runs(session_id, started_at DESC);

CREATE INDEX idx_turns_run_seq ON turns(run_id, seq);

CREATE INDEX idx_turn_parts_turn_id ON turn_parts(turn_id, id);

CREATE INDEX idx_turn_parts_session_id ON turn_parts(session_id);

CREATE UNIQUE INDEX idx_artifacts_sha256 ON artifacts(sha256);

CREATE INDEX idx_provider_payloads_run_sequence ON provider_payloads(run_id, sequence);

CREATE INDEX idx_provider_payloads_session ON provider_payloads(session_id, created_at);

CREATE INDEX idx_turn_parts_text_turn_id ON turn_parts_text(turn_id);

CREATE TRIGGER trg_turn_parts_text_insert
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

CREATE TRIGGER trg_turn_parts_text_update
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

CREATE TRIGGER trg_turn_parts_text_delete
AFTER DELETE ON turn_parts
BEGIN
    DELETE FROM turn_parts_text WHERE part_id = OLD.id;
END;

CREATE TRIGGER trg_turn_parts_fts_insert
AFTER INSERT ON turn_parts_text
BEGIN
    INSERT INTO turn_parts_fts (rowid, part_id, turn_id, text)
    VALUES (NEW.rowid, NEW.part_id, NEW.turn_id, NEW.text);
    INSERT INTO turn_parts_fts_trigram (rowid, part_id, turn_id, text)
    VALUES (NEW.rowid, NEW.part_id, NEW.turn_id, NEW.text);
END;

CREATE TRIGGER trg_turn_parts_fts_update
AFTER UPDATE ON turn_parts_text
BEGIN
    DELETE FROM turn_parts_fts WHERE rowid = OLD.rowid;
    DELETE FROM turn_parts_fts_trigram WHERE rowid = OLD.rowid;
    INSERT INTO turn_parts_fts (rowid, part_id, turn_id, text)
    VALUES (NEW.rowid, NEW.part_id, NEW.turn_id, NEW.text);
    INSERT INTO turn_parts_fts_trigram (rowid, part_id, turn_id, text)
    VALUES (NEW.rowid, NEW.part_id, NEW.turn_id, NEW.text);
END;

CREATE TRIGGER trg_turn_parts_fts_delete
AFTER DELETE ON turn_parts_text
BEGIN
    DELETE FROM turn_parts_fts WHERE rowid = OLD.rowid;
    DELETE FROM turn_parts_fts_trigram WHERE rowid = OLD.rowid;
END;

