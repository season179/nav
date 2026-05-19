CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS session (
    id TEXT PRIMARY KEY,
    cwd TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    title TEXT,
    name TEXT,
    profile TEXT,
    provider_meta TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    cost_currency TEXT NOT NULL DEFAULT 'USD',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    tokens_input INTEGER NOT NULL DEFAULT 0,
    tokens_output INTEGER NOT NULL DEFAULT 0,
    tokens_input_cached INTEGER NOT NULL DEFAULT 0,
    tokens_reasoning INTEGER NOT NULL DEFAULT 0,
    cost_micros_reported INTEGER NOT NULL DEFAULT 0,
    turns_with_reported_cost INTEGER NOT NULL DEFAULT 0,
    turns_total INTEGER NOT NULL DEFAULT 0,
    parent_id TEXT REFERENCES session(id),
    fork_point_seq INTEGER
);

-- idx_session_parent is created in the v3 migration step rather than here,
-- so init.sql can run against a v1 database whose session table predates
-- the parent_id column without `CREATE INDEX` failing.

CREATE INDEX IF NOT EXISTS idx_session_cwd_updated_at
    ON session(cwd, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_session_provider_updated_at
    ON session(provider, updated_at DESC);

CREATE TABLE IF NOT EXISTS event (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
);

CREATE TABLE IF NOT EXISTS turn (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    turn_index INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    model TEXT NOT NULL,
    tokens_input INTEGER NOT NULL DEFAULT 0,
    tokens_output INTEGER NOT NULL DEFAULT 0,
    tokens_input_cached INTEGER NOT NULL DEFAULT 0,
    tokens_reasoning INTEGER NOT NULL DEFAULT 0,
    cost_micros INTEGER,
    cost_currency TEXT NOT NULL DEFAULT 'USD',
    cost_source TEXT NOT NULL DEFAULT 'unreported',
    error TEXT,
    PRIMARY KEY (session_id, turn_index)
);

CREATE TABLE IF NOT EXISTS approval (
    session_id    TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    approval_id   TEXT NOT NULL,
    requested_at  INTEGER NOT NULL,
    decided_at    INTEGER,
    tool          TEXT NOT NULL,
    command       TEXT,
    path          TEXT,
    reason        TEXT NOT NULL,
    decision      TEXT,
    rule          TEXT,
    PRIMARY KEY (session_id, approval_id)
);

CREATE INDEX IF NOT EXISTS idx_approval_session
    ON approval(session_id, requested_at);

CREATE TABLE IF NOT EXISTS label (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    label TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, label)
);

CREATE INDEX IF NOT EXISTS idx_label_name
    ON label(label);

CREATE VIRTUAL TABLE IF NOT EXISTS event_fts USING fts5(
    session_id UNINDEXED,
    seq UNINDEXED,
    kind UNINDEXED,
    text
);

CREATE TRIGGER IF NOT EXISTS event_fts_ai
AFTER INSERT ON event
WHEN NEW.kind IN ('user_message', 'assistant_message_done', 'assistant_message_delta')
BEGIN
    INSERT INTO event_fts (session_id, seq, kind, text)
    VALUES (
        NEW.session_id,
        NEW.seq,
        NEW.kind,
        COALESCE(json_extract(NEW.data, '$.text'), '')
    );
END;

CREATE TRIGGER IF NOT EXISTS event_fts_ad
AFTER DELETE ON event
WHEN OLD.kind IN ('user_message', 'assistant_message_done', 'assistant_message_delta')
BEGIN
    DELETE FROM event_fts
    WHERE session_id = OLD.session_id AND seq = OLD.seq;
END;
