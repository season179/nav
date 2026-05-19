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
    turns_total INTEGER NOT NULL DEFAULT 0
);

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
