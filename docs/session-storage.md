# Session storage (SQLite)

Status: planned. Canonical transcript storage for the agent loop, with
per-request encoding for multiple LLM API shapes.

Related: [architecture.md](./architecture.md), [model-provider-settings.md](./model-provider-settings.md).

## Problem

The agent loop is stateless at the provider: every iteration must reconstruct
prior context. Users may change model or provider mid-session (e.g. OpenAI Chat
Completions → Anthropic Messages). History must survive that switch without
corrupting the thread.

Legacy OpenAI `/v1/completions` (`prompt` + `choices[].text`) is out of scope.
Nav targets three API shapes:

| API | Endpoint | Wire input | Wire output |
| --- | --- | --- | --- |
| OpenAI Chat Completions | `POST /v1/chat/completions` | `messages[]` | `choices[].message` |
| OpenAI Responses | `POST /v1/responses` | `input` / `instructions` | `output[]`, `output_text` |
| Anthropic Messages | `POST /v1/messages` | `messages[]` + `system` | `content` blocks |

## Principles

1. **One canonical ledger in SQLite** — not three provider-native copies per turn.
2. **Encode on read** — load canonical turns, then `Encoder(api_kind)` for the
   model resolved *this* iteration.
3. **Decode on write** — provider response → canonical `Turn` → append in one
   transaction.
4. **Runs are the agent-loop unit** — a session has many runs; turns belong to a
   run and are strictly ordered by `seq`.
5. **Provider continuation is optional cache** — e.g. OpenAI Responses
   `previous_response_id` in `provider_state`, never the source of truth.

```text
Persist (always)              Reconstruct (per turn)
───────────────               ──────────────────────
SQLite: canonical turns  →    ModelResolver → ApiKind
                                 Encoder(api_kind) → provider JSON
                                 (HTTP in nav-server only)
```

## Crate layout

| Location | Responsibility |
| --- | --- |
| `nav-harness::sessions::canonical` | `Turn`, `Part`, `TurnMeta` |
| `nav-harness::sessions::store` | `SessionStore` trait + SQLite implementation |
| `nav-harness::sessions::migrate` | SQL migrations |
| `nav-harness::models::encode` | Build provider request from canonical turns (no HTTP) |
| `nav-harness::models::decode` | Parse provider response into canonical turns |
| `nav-types` | `SessionId`, `RunId`, `MessageId`, `ToolCallId` (UUIDv7) |

Extend `models::api::ApiKind` when encoders land:

```rust
pub enum ApiKind {
    OpenAiChatCompletions,
    OpenAiResponses,
    AnthropicMessages,
}
```

**Dependencies (workspace):** `rusqlite` (bundled SQLite), `serde_json`, error
crate as used elsewhere. SQL stays in `nav-harness`; `nav-server` opens the DB
path or receives a `SessionStore` handle.

**Database file:** `{data_dir}/nav.db` (e.g. `~/.nav/nav.db` or workspace
`.nav/nav.db`). On open:

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
```

## Canonical domain model

Persisted as JSON in `turns.parts_json` and `turns.meta_json`, validated on read
in Rust.

```rust
enum TurnRole { User, Assistant, Tool }

struct Turn {
    id: MessageId,
    run_id: RunId,
    seq: u32,           // 0..n-1 within run
    role: TurnRole,
    parts: Vec<Part>,
    meta: TurnMeta,
    created_at: i64,    // unix millis
}

enum Part {
    Text { text: String },
    Image { mime: String, source: ImageSource },
    ToolCall { id: ToolCallId, name: String, arguments: serde_json::Value },
    ToolResult { call_id: ToolCallId, content: String, is_error: bool },
    Thinking { text: String, provider_hint: Option<String> },
}

struct TurnMeta {
    model_provider: Option<String>,
    model_id: Option<String>,
    api_kind: Option<ApiKind>,
    finish_reason: Option<String>,
    usage: Option<TokenUsage>,
}
```

**Conventions**

- `ToolCallId` is nav-owned (UUIDv7); encoders map to provider ids each request.
- Pick one tool-result shape and keep encoders consistent (recommended:
  `ToolResult` parts on a `user` turn for Anthropic-friendly encoding).
- Images: `ImageSource::FileRef { attachment_id }` with bytes in `attachments`.

`TurnMeta` is audit and UI only — not replayed as wire format.

## SQLite schema

### `schema_migrations`

```sql
CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY NOT NULL,
    applied_at  INTEGER NOT NULL
);
```

### `sessions`

```sql
CREATE TABLE sessions (
    id              TEXT PRIMARY KEY NOT NULL,
    title           TEXT,
    workspace_root  TEXT,
    system_prompt   TEXT,
    settings_json   TEXT NOT NULL DEFAULT '{}',
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
```

`settings_json` holds the **current** model selection (and related session
prefs), e.g.:

```json
{
  "modelRef": { "provider": "anthropic", "model": "claude-sonnet-4-6" }
}
```

Changing model mid-session updates this field only; existing turns stay
canonical.

### `runs`

```sql
CREATE TABLE runs (
    id              TEXT PRIMARY KEY NOT NULL,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    status          TEXT NOT NULL,
    trigger         TEXT,
    started_at      INTEGER NOT NULL,
    finished_at     INTEGER,
    error_json      TEXT
);

CREATE INDEX idx_runs_session_started ON runs(session_id, started_at DESC);
```

`status`: `pending` | `running` | `completed` | `failed` | `cancelled`.

### `turns`

```sql
CREATE TABLE turns (
    id              TEXT PRIMARY KEY NOT NULL,
    run_id          TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    seq             INTEGER NOT NULL,
    role            TEXT NOT NULL,
    parts_json      TEXT NOT NULL,
    meta_json       TEXT NOT NULL DEFAULT '{}',
    created_at      INTEGER NOT NULL,
    UNIQUE (run_id, seq)
);

CREATE INDEX idx_turns_run_seq ON turns(run_id, seq);
```

`role`: `user` | `assistant` | `tool`.

v1 stores `parts` as JSON; normalized `turn_parts` rows are optional later if
search or analytics need them.

### `provider_state`

Optional cache for provider-specific chaining (not authoritative history).

```sql
CREATE TABLE provider_state (
    run_id          TEXT PRIMARY KEY NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    api_kind        TEXT NOT NULL,
    state_json      TEXT NOT NULL
);
```

Example `state_json` for OpenAI Responses:

```json
{ "previous_response_id": "resp_..." }
```

Clear or ignore this row when `ApiKind` changes for the session/run.

### `attachments`

```sql
CREATE TABLE attachments (
    id              TEXT PRIMARY KEY NOT NULL,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    mime            TEXT NOT NULL,
    sha256          TEXT NOT NULL,
    path            TEXT NOT NULL,
    size_bytes      INTEGER NOT NULL,
    created_at      INTEGER NOT NULL
);
```

Blob files live under `{data_dir}/blobs/`; DB stores path + hash.

## `SessionStore` API

```rust
pub struct SessionStore { /* rusqlite::Connection or Arc<Mutex<_>> */ }

impl SessionStore {
    pub fn open(path: &Path) -> Result<Self>;
    pub fn migrate(&self) -> Result<()>;

    pub fn create_session(&self, id: SessionId, opts: CreateSession) -> Result<()>;
    pub fn get_session(&self, id: &SessionId) -> Result<SessionRow>;
    pub fn update_session_settings(&self, id: &SessionId, settings: SessionSettings) -> Result<()>;

    pub fn start_run(&self, run: StartRun) -> Result<()>;
    pub fn finish_run(&self, run_id: &RunId, status: RunStatus, error: Option<RunError>) -> Result<()>;

    pub fn append_turn(&self, turn: Turn) -> Result<()>;
    pub fn append_turns(&self, turns: &[Turn]) -> Result<()>;

    pub fn list_turns_for_run(&self, run_id: &RunId) -> Result<Vec<Turn>>;
    pub fn list_turns_for_session(&self, session_id: &SessionId, limit: usize) -> Result<Vec<Turn>>;

    pub fn get_provider_state(&self, run_id: &RunId) -> Result<Option<ProviderState>>;
    pub fn set_provider_state(&self, run_id: &RunId, state: ProviderState) -> Result<()>;
}
```

**Sequence numbers:** assign `seq = MAX(seq) + 1` inside the same transaction as
insert; do not rely on in-memory counters alone across restarts.

## Agent loop flow

```text
1. resolve_model(session.settings.modelRef) → ResolvedModelConfig { api_kind, ... }
2. start_run(session_id) → run_id
3. loop:
     turns = list_turns_for_run(run_id)
     optional: context::truncate(turns, budget) → turns_for_model
     request = encode(api_kind, turns, system_prompt, tools, compat)
     if api_kind == OpenAiResponses:
         attach previous_response_id from provider_state when valid
     response = http_client.call(request)   // nav-server only
     new_turns = decode(api_kind, response)
     BEGIN; append_turns(new_turns); update provider_state; COMMIT
     if terminal: break
4. finish_run(run_id, completed | failed)
```

### Mid-session model change

1. User selects a different provider/model → `update_session_settings`.
2. Next iteration uses new `ApiKind` and encoder on **unchanged** canonical turns.
3. Drop or invalidate `provider_state` when `api_kind` no longer matches.

No migration of historical rows required.

## Encoder / decoder boundary

| Step | Module | Input | Output |
| --- | --- | --- | --- |
| Load | `SessionStore` | `run_id` | `Vec<Turn>` |
| Truncate | `context` | turns + token budget | `Vec<Turn>` |
| Encode | `models::encode` | `ApiKind`, turns, compat | `EncodedRequest` |
| Decode | `models::decode` | response + `ApiKind` | `Vec<Turn>` |
| Save | `SessionStore` | `Vec<Turn>` | SQLite |

Storage never stores `EncodedRequest` or raw provider response bodies as the
ledger (optional debug tables are out of scope for v1).

### Encoding failures on old turns

1. Drop unmappable `Thinking` parts.
2. Degraded mode: render prior tool activity as text in a user turn.
3. Last resort: compaction summary turn; mark superseded turns compacted (phase 2).

## Context limits and compaction (phase 2)

Add without breaking v1 readers:

```sql
ALTER TABLE turns ADD COLUMN flags INTEGER NOT NULL DEFAULT 0;
-- bit 0: compacted (excluded from encode by default)
-- bit 1: synthetic summary
```

Or a `run_compactions` table mapping `through_seq` → summary `turn_id`.
Encoder loads non-compacted turns plus the latest summary.

## Concurrency and durability

- SQLite single-writer is sufficient for desktop agent use; wrap connection in
  `Mutex` if shared across tasks.
- WAL allows UI/history reads during loop writes.
- One transaction per agent iteration (append turns + provider state).
- `ON DELETE CASCADE` from sessions → runs → turns.

## Protocol / UI boundary

`nav-protocol` events expose simplified message shapes for the TUI (`message_id`,
`role`, text deltas). Canonical `Part` lists remain in harness storage for
reconstruction. See [architecture.md](./architecture.md) for SSE/event IDs.

## Implementation phases

| Phase | Deliverable |
| --- | --- |
| 0 | Migrations, `SessionStore::open` / `migrate`, sessions + runs + turns CRUD |
| 1 | Canonical `Turn` / `Part` types + JSON round-trip tests |
| 2 | `encode` / `decode` for `OpenAiChatCompletions` only |
| 3 | Agent loop wired to store + encode path (nav-server HTTP) |
| 4 | `AnthropicMessages` + `OpenAiResponses` encoders; `provider_state` |
| 5 | Attachments blob store + compaction flags |

## Anti-patterns

- Storing separate `openai_request`, `anthropic_request`, and `responses_request`
  JSON per turn (drift, 3× size, unclear source of truth).
- Using only OpenAI `previous_response_id` without a local canonical ledger.
- Appending provider-native messages directly from the client without decode.

## Open questions

- Default `data_dir` resolution: global vs per-workspace DB.
- Whether `list_turns_for_session` spans all runs or only the active run for UI.
- Retention policy for old runs/attachments (not required for v1).
