# Session storage (SQLite)

Status: planned. Canonical transcript storage for the agent loop, with
per-request encoding for multiple LLM API shapes.

Informed by analysis of OpenCode's session/message/part system. See
[Research notes](#research-notes-opencode) at the bottom for rationale.

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
6. **Parts are rows, not JSON blobs** — each part is a separate row so streaming
   deltas, compaction, and partial deletes work without rewriting entire turns.
7. **Session-level aggregates** — cost and token counts accumulated on the
   `sessions` row for O(1) queries, not recomputed from parts.

```text
Persist (always)              Reconstruct (per turn)
───────────────               ──────────────────────
SQLite: canonical turns  →    ModelResolver → ApiKind
         + turn_parts          Encoder(api_kind) → provider JSON
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
| `nav-types` | `SessionId`, `RunId`, `MessageId`, `ToolCallId` (prefixed monotonic IDs) |

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
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
PRAGMA cache_size = -64000;
```

`busy_timeout` prevents immediate failures under concurrent access.
`synchronous = NORMAL` is safe with WAL and faster than FULL.
`cache_size = -64000` gives SQLite 64 MB page cache.

## ID scheme

All IDs are prefixed, monotonic, and human-readable:

| Prefix | Type | Direction | Example |
| --- | --- | --- | --- |
| `ses_` | SessionId | descending (recent sorts first) | `ses_a1b2c3d4e5f6...` |
| `run_` | RunId | ascending | `run_7f8e9d0c1a2b...` |
| `msg_` | MessageId (= TurnId) | ascending | `msg_3b4c5d6e7f8a...` |
| `prt_` | PartId | ascending | `prt_9a0b1c2d3e4f...` |
| `tool_` | ToolCallId | ascending | `tool_5e6f7a8b9c0d...` |

**Ascending** = newer IDs are lexicographically larger. **Descending** = newer
IDs are lexicographically smaller (so `ORDER BY id` gives recent-first without
needing a separate timestamp column in the index).

IDs embed a millisecond timestamp (like UUIDv7) so they are temporally ordered
and can be extracted for display. The prefix makes logs and debug output
immediately legible.

**Pagination** uses cursor-based paging on `(created_at, id)` — stable across
concurrent writes, no offset drift:

```sql
WHERE created_at < ? OR (created_at = ? AND id < ?)
ORDER BY created_at DESC, id DESC
LIMIT ?
```

## Canonical domain model

### Turn (message envelope)

```rust
enum TurnRole { User, Assistant }

struct Turn {
    id: MessageId,
    run_id: RunId,
    seq: u32,           // 0..n-1 within run
    role: TurnRole,
    meta: TurnMeta,
    created_at: i64,    // unix millis
}
```

A `Turn` is the envelope. Its `Part` rows live in the separate `turn_parts`
table, loaded via join. This split means:

- Streaming deltas patch one part row (no full-turn rewrite).
- Compaction clears old tool output by flagging individual parts.
- Deleting a single tool call is a row delete, not JSON surgery.

### Part

```rust
enum Part {
    Text {
        text: String,
        synthetic: Option<bool>,      // true if generated by nav, not the user
    },
    Image {
        mime: String,
        source: ImageSource,          // FileRef { attachment_id } or inline bytes
    },
    ToolCall {
        id: ToolCallId,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: ToolCallId,
        content: String,
        is_error: bool,
    },
    Thinking {
        text: String,
        provider_hint: Option<String>,
    },
    StepStart {
        snapshot: Option<String>,     // filesystem snapshot at step boundary
    },
    StepFinish {
        reason: String,               // "stop", "tool_use", etc.
        cost: f64,
        tokens: TokenUsage,
        snapshot: Option<String>,
    },
    Compaction {
        auto: bool,                   // triggered automatically vs manually
        tail_start_id: Option<MessageId>,  // where retained history begins
    },
    Retry {
        attempt: u32,
        error_json: serde_json::Value,
    },
    Snapshot {
        snapshot_id: String,          // filesystem checkpoint reference
    },
}
```

**StepStart / StepFinish** — Anthropic and OpenAI Responses can produce multiple
tool-use rounds in a single assistant turn. These part variants mark those
boundaries within a turn. Encoders that don't model steps (Chat Completions)
simply ignore them. Each `StepFinish` carries per-step cost/tokens for
fine-grained accounting.

**Compaction** — marks a compaction boundary on a user turn. `tail_start_id`
points to the first turn to retain verbatim after the summary. The encoder
loads: [compaction-marker turn → summary turn → turns from tail_start_id
onward].

**Retry** — records failed attempts before the successful one. Visible in the
UI for debugging, excluded from encoding by default.

**Conventions**

- `ToolCallId` is nav-owned; encoders map to provider ids each request.
- Tool results go on the assistant turn (not a separate `user` turn). The
  Anthropic encoder restructures them into the `tool_result` content blocks
  Anthropic expects.
- Images: `ImageSource::FileRef { attachment_id }` with bytes in `attachments`.
- `synthetic: true` on Text parts marks nav-generated text (e.g. compaction
  follow-ups, attachment descriptions) so the UI can style them differently.

### TurnMeta

```rust
struct TurnMeta {
    model_provider: Option<String>,
    model_id: Option<String>,
    api_kind: Option<ApiKind>,
    finish_reason: Option<String>,
    usage: Option<TokenUsage>,
    parent_id: Option<MessageId>,   // for assistant turns: the triggering user turn
}
```

`TurnMeta` is audit and UI only — not replayed as wire format. `parent_id`
enables the UI to reconstruct conversation threading.

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
    parent_id       TEXT REFERENCES sessions(id),    -- fork chain
    version         TEXT NOT NULL,                    -- app version at creation
    slug            TEXT,                             -- human-readable URL slug
    -- Session-level cost/token accumulation
    cost            REAL NOT NULL DEFAULT 0,
    tokens_input    INTEGER NOT NULL DEFAULT 0,
    tokens_output   INTEGER NOT NULL DEFAULT 0,
    tokens_reasoning INTEGER NOT NULL DEFAULT 0,
    tokens_cache_read  INTEGER NOT NULL DEFAULT 0,
    tokens_cache_write INTEGER NOT NULL DEFAULT 0,
    -- Soft delete and state
    time_archived   INTEGER,                         -- null = active
    time_compacting INTEGER,                         -- set during compaction
    -- Revert (undo assistant actions)
    revert_json     TEXT,                             -- { message_id, part_id?, snapshot?, diff? }
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

**Fork chain:** `parent_id` links to the session this was forked from. Forking
copies turns up to a given message into a new session with remapped IDs.

**Cost/tokens:** accumulated via SQL arithmetic (`cost = cost + ?`) on every
`StepFinish` part upsert. Reversed when parts are updated/replaced. Gives O(1)
cost queries for the TUI and cost-limit checks.

**Revert:** stores a snapshot reference so the user can undo the last assistant
turn and restore filesystem state. Cleared when the user continues the
conversation.

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
    meta_json       TEXT NOT NULL DEFAULT '{}',
    created_at      INTEGER NOT NULL,
    UNIQUE (run_id, seq)
);

CREATE INDEX idx_turns_run_seq ON turns(run_id, seq);
```

`role`: `user` | `assistant`.

Note: no `parts_json` column. Parts live in `turn_parts`.

### `turn_parts`

```sql
CREATE TABLE turn_parts (
    id              TEXT PRIMARY KEY NOT NULL,
    turn_id         TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
    session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    type            TEXT NOT NULL,           -- "text", "tool_call", "tool_result", etc.
    data_json       TEXT NOT NULL,           -- type-specific payload
    compacted_at    INTEGER,                -- null = live; set = tool output cleared
    created_at      INTEGER NOT NULL
);

CREATE INDEX idx_turn_parts_turn_id ON turn_parts(turn_id, id);
CREATE INDEX idx_turn_parts_session_id ON turn_parts(session_id);
```

Each part is its own row. `type` is the discriminator; `data_json` holds the
type-specific fields. `compacted_at` is `NULL` for live parts; when set, it
means old tool output was cleared to save context space. The timestamp enables
"when was this compacted?" queries.

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

    // Sessions
    pub fn create_session(&self, id: SessionId, opts: CreateSession) -> Result<()>;
    pub fn get_session(&self, id: &SessionId) -> Result<SessionRow>;
    pub fn update_session_settings(&self, id: &SessionId, settings: SessionSettings) -> Result<()>;
    pub fn update_session_cost(&self, id: &SessionId, delta_cost: f64, delta_tokens: TokenDelta) -> Result<()>;

    // Runs
    pub fn start_run(&self, run: StartRun) -> Result<()>;
    pub fn finish_run(&self, run_id: &RunId, status: RunStatus, error: Option<RunError>) -> Result<()>;

    // Turns (envelope + parts in one transaction)
    pub fn append_turn(&self, turn: Turn, parts: Vec<Part>) -> Result<()>;
    pub fn append_turns(&self, turns_with_parts: &[(Turn, Vec<Part>)]) -> Result<()>;

    // Reads (join turns + turn_parts, reconstruct Vec<(Turn, Vec<Part>)>)
    pub fn list_turns_for_run(&self, run_id: &RunId) -> Result<Vec<(Turn, Vec<Part>)>>;
    pub fn list_turns_for_session(&self, cursor: Option<Cursor>, limit: usize) -> Result<TurnPage>;

    // Parts (for streaming deltas and compaction)
    pub fn update_part(&self, part: Part) -> Result<()>;
    pub fn update_part_delta(&self, turn_id: &MessageId, part_id: &PartId, field: &str, delta: &str) -> Result<()>;
    pub fn remove_part(&self, session_id: &SessionId, turn_id: &MessageId, part_id: &PartId) -> Result<()>;
    pub fn compact_part(&self, part_id: &PartId) -> Result<()>;  // sets compacted_at

    // Provider state
    pub fn get_provider_state(&self, run_id: &RunId) -> Result<Option<ProviderState>>;
    pub fn set_provider_state(&self, run_id: &RunId, state: ProviderState) -> Result<()>;

    // Fork
    pub fn fork_session(&self, source: &SessionId, through_message: Option<&MessageId>) -> Result<SessionRow>;
}
```

**Sequence numbers:** assign `seq = MAX(seq) + 1` inside the same transaction as
insert; do not rely on in-memory counters alone across restarts.

**Cost accumulation:** `update_session_cost` does `SET cost = cost + ?` etc.
inside the same transaction as `append_turns`. When a part is updated or
replaced, reverse the old delta first, then apply the new one.

**Cursor:** `TurnPage` returns `{ items, more, cursor: Option<(i64, String)> }`
where the cursor is `(created_at, id)` for stable cursor-based pagination.

**Streaming deltas:** `update_part_delta` appends a string to a field within
`data_json` without read-modify-write at the Rust level — uses a SQL update with
`json_set` or a raw append, depending on the field.

## Agent loop flow

```text
1. resolve_model(session.settings.modelRef) → ResolvedModelConfig { api_kind, ... }
2. start_run(session_id) → run_id
3. loop:
     turns = list_turns_for_run(run_id)
     optional: prune_compacted_parts(turns)    -- clear old tool output
     optional: context::truncate(turns, budget) → turns_for_model
     request = encode(api_kind, turns, system_prompt, tools, compat)
     if api_kind == OpenAiResponses:
         attach previous_response_id from provider_state when valid
     response = http_client.call(request)   // nav-server only
     new_turns_with_parts = decode(api_kind, response)
     BEGIN;
       append_turns(new_turns_with_parts);
       update provider_state;
       update session cost/tokens from StepFinish parts;
     COMMIT
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
| Load | `SessionStore` | `run_id` | `Vec<(Turn, Vec<Part>)>` |
| Prune | `compaction` | turns | turns with old tool output cleared |
| Truncate | `context` | turns + token budget | `Vec<(Turn, Vec<Part>)>` |
| Encode | `models::encode` | `ApiKind`, turns, compat | `EncodedRequest` |
| Decode | `models::decode` | response + `ApiKind` | `Vec<(Turn, Vec<Part>)>` |
| Save | `SessionStore` | `Vec<(Turn, Vec<Part>)>` | SQLite |

Storage never stores `EncodedRequest` or raw provider response bodies as the
ledger (optional debug tables are out of scope for v1).

### How the three API shapes map to canonical parts

All three encoders consume the same `Vec<(Turn, Vec<Part>)>`. They differ only
in how they flatten/group the parts into wire format:

| Canonical part | Chat Completions | Responses API | Anthropic Messages |
| --- | --- | --- | --- |
| `Text` | `message.content` | `output_text` | `content[].text` |
| `Image` | `message.content[].image_url` | `input[].image` | `content[].image` |
| `ToolCall` | `message.tool_calls[]` | `output[].function_call` | `content[].tool_use` |
| `ToolResult` | `message.role=tool` | `input[].function_call_output` | `content[].tool_result` |
| `Thinking` | dropped | dropped | `content[].thinking` |
| `StepStart/Finish` | ignored | mapped to separate `output[]` entries | mapped to multi-step `content` |
| `Compaction` | rendered as user text | rendered as user text | rendered as user text |
| `Retry` | excluded | excluded | excluded |

### Encoding failures on old turns

1. Drop unmappable `Thinking` parts (e.g. when switching from Anthropic to a
   provider that doesn't support thinking).
2. Degraded mode: render prior tool activity as text in a user turn.
3. Last resort: compaction summary turn; mark superseded turns compacted.

### Switching between API shapes mid-session

Because canonical storage is API-agnostic, switching is seamless:

1. User changes model from Claude (Anthropic) to GPT-4o (Chat Completions).
2. The same `Vec<(Turn, Vec<Part>)>` is loaded from storage.
3. The Chat Completions encoder simply drops `Thinking` parts and maps
   `ToolResult` parts to `role: "tool"` messages — no data migration needed.
4. Reverse direction works identically.

## Compaction

Context windows fill up. Compaction shrinks the turn history sent to the model
while preserving the canonical ledger intact.

### Pruning (cheap, frequent)

Walk backward through tool-result parts. Keep a token budget of recent results
(`PRUNE_PROTECT = 40K` tokens). For older results, set `compacted_at` on the
part — the encoder replaces the content with `"[Old tool result content
cleared]"`. No rows are deleted. No summarization LLM call needed.

```text
for each tool_result part, newest first:
    if tokens in budget: keep as-is
    else: set compacted_at = now
```

Protected tools (e.g. `skill`) are never pruned — their output may be needed
for context even when old.

### Summarization (expensive, on overflow or manual)

When the total turn history exceeds the usable context window:

1. **Select** a "head" (turns to summarize) and "tail" (recent turns to keep
   verbatim). Tail size is configurable (`tail_turns`, default 2). The tail
   budget is a fraction of usable context.
2. **Generate summary** using a compaction agent over the head turns, building
   on any previous summary (incremental).
3. **Write compaction turn**: a user turn with a `Compaction` part
   (`tail_start_id` → first retained turn) followed by an assistant turn
   containing the summary as a `Text` part.
4. **On replay**, the encoder loads: [compaction-marker turn → summary → turns
   from `tail_start_id` onward].

```text
Loaded for encoding:
  ┌─────────────────────┐
  │ Compaction (user)   │ ← meta: "what did we do so far?"
  │ Summary (assistant) │ ← LLM-generated summary text
  ├─────────────────────┤
  │ tail_start → ...    │ ← retained turns (verbatim)
  │ current user turn   │
  └─────────────────────┘
```

### Overflow handling

When a request exceeds the provider's size limit even after pruning:

1. Auto-compact with `overflow: true`.
2. Strip media attachments from the compacted context.
3. Replay the triggering user message after compaction with a synthetic
   continuation prompt.

### Compaction schema support

The `turn_parts.compacted_at` column (already in the schema) enables this
without migration. No additional flags needed.

## Concurrency and durability

- SQLite single-writer is sufficient for desktop agent use; wrap connection in
  `Mutex` if shared across tasks.
- WAL allows UI/history reads during loop writes.
- One transaction per agent iteration (append turns + parts + provider state +
  cost accumulation).
- `ON DELETE CASCADE` from sessions → runs → turns → turn_parts.
- Transactions use `IMMEDIATE` behavior to avoid deadlocks when the write
  connection is contended.

## Protocol / UI boundary

`nav-protocol` events expose simplified message shapes for the TUI (`message_id`,
`role`, text deltas). Canonical `Part` lists remain in harness storage for
reconstruction. See [architecture.md](./architecture.md) for SSE/event IDs.

**Streaming:** the TUI receives part-level deltas (`update_part_delta`) for
real-time text streaming. Individual parts are streamed as they arrive, not
whole turns.

## Implementation phases

| Phase | Deliverable |
| --- | --- |
| 0 | Migrations, `SessionStore::open` / `migrate`, sessions + runs + turns + turn_parts CRUD |
| 1 | Canonical `Turn` / `Part` types + row round-trip tests |
| 2 | `encode` / `decode` for `OpenAiChatCompletions` only |
| 3 | Agent loop wired to store + encode path (nav-server HTTP) |
| 4 | `AnthropicMessages` + `OpenAiResponses` encoders; `provider_state`; step handling |
| 5 | Attachments blob store |
| 6 | Pruning (tool output compaction via `compacted_at`) |
| 7 | Summarization compaction + overflow handling |
| 8 | Fork, revert, session-level cost display |

## Anti-patterns

- Storing separate `openai_request`, `anthropic_request`, and `responses_request`
  JSON per turn (drift, 3× size, unclear source of truth).
- Using only OpenAI `previous_response_id` without a local canonical ledger.
- Appending provider-native messages directly from the client without decode.
- Storing `parts` as a JSON blob inside the turn row (forces full rewrite on
  every streaming delta, compaction, or partial delete).
- Summing token/cost from parts on every query instead of accumulating at the
  session level.

## Open questions

- Default `data_dir` resolution: global vs per-workspace DB.
- Whether `list_turns_for_session` spans all runs or only the active run for UI.
- Retention policy for old runs/attachments (not required for v1).
- Whether event sourcing (typed events + projectors → read models) should be
  introduced from the start or added later. OpenCode uses this pattern
  extensively for sync and replay, but it adds complexity. Recommendation: keep
  write paths narrow (all writes go through `SessionStore` methods) so a
  projector layer can be slotted in later without refactoring.

## Research notes: OpenCode

Key patterns borrowed from OpenCode's session storage (TypeScript/Drizzle):

1. **Message/Part split** — OpenCode has `message` (envelope) and `part`
   (content) as separate tables. This enables streaming deltas on individual
   parts, partial deletes, and compaction without JSON surgery. Nav adopts this
   as `turns` + `turn_parts`.

2. **Session-level cost accumulation** — OpenCode accumulates cost/tokens on the
   session row via SQL arithmetic (`cost = cost + ?`), reversed on part
   replacement. Adopted for nav's `sessions` table.

3. **Compaction** — OpenCode has a three-tier compaction system:
   - *Pruning*: sets a `compacted_at` timestamp on old tool-result parts,
     replacing content with a placeholder. Cheap, no LLM call.
   - *Summarization*: generates an incremental markdown summary over old turns,
     keeps a configurable tail of recent turns verbatim.
   - *Overflow*: auto-compacts on context limit errors, strips media, replays
     the user message.
   Adopted for nav's compaction design.

4. **Prefixed monotonic IDs** — OpenCode uses `ses_`, `msg_`, `prt_` prefixed
   IDs with embedded timestamps and sort order (ascending or descending).
   Adopted for nav's ID scheme.

5. **StepStart/StepFinish** — OpenCode models multi-step responses (Anthropic
   extended thinking, multiple tool rounds per response) with explicit step
   boundary parts. Adopted as canonical part variants.

6. **Fork and revert** — OpenCode supports forking a session (copy turns up to
   a point into a new session with remapped IDs) and reverting (store a
   filesystem snapshot reference to undo assistant actions). Planned for nav
   phase 8.

7. **DB pragmas** — OpenCode uses `busy_timeout = 5000`, `synchronous = NORMAL`,
   `cache_size = -64000` in addition to WAL + foreign_keys. Adopted for nav's
   DB open sequence.

8. **Cursor-based pagination** — OpenCode pages messages via
   `(time_created, id)` cursors with `DESC` ordering. Adopted for nav's
   `list_turns_for_session`.
