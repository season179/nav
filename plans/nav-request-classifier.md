# Nav Request Classifier (side-channel)

Status: finalized plan, co-designed with Codex gpt-5.5 (2026-06-28). Not yet implemented.

## Goal

Every time the user sends a message to Nav, a **DeepSeek V4 Flash** side process classifies
the request and shows two labels **under that user message**:

1. **is-planning** — is this a "to-do planning" request (decompose / design an approach / plan
   tasks) or a direct request? Displayed as `Planning` vs `Direct`.
2. **difficulty** — `low` | `medium` | `high`.

Hard constraint from the user: it is a **separate asynchronous process that does NOT influence
the main conversation** — purely informational. The main Nav chat stream is untouched.

## Architecture decision

**Use the existing lightweight Flue *workflow* pattern (the session-title clone), NOT the
`deepseek-flash` delegate agent.** The user said "agent," but Nav's `agents/deepseek-flash.ts`
delegate provisions a **per-call git worktree** (`shared/worktrees.ts`) — heavyweight and wrong
for a fast classifier. `workflows/session-title.ts` already runs `deepseek/deepseek-v4-flash`
with `thinkingLevel: "minimal"`, `tools: []`, and valibot structured output via
`session.prompt(prompt, { result })` — **same model, no sandbox, no worktree**. That is exactly
the mechanism we want.

**Trigger from the frontend, not the backend.** Reasons: (a) it must never touch the
`/api/agents/nav/:id` main stream; (b) the classify endpoint can live *outside*
`requireCodexProvider`, so chips work even if Codex auth is down; (c) the message id and the
render location both live on the frontend. The flow runs concurrently with the assistant's
response and feeds nothing back into it.

```
User submit
  -> useFlueAgent main conversation (UNCHANGED)
  -> after historyReady, frontend observes a NEW user message id
  -> POST /api/sessions/:id/classify { messageId, text, priorAssistant? }
  -> backend: return existing row if present, else run request-classifier workflow (loopback)
  -> upsert nav_message_classifications
  -> frontend renders chips under that user bubble
```

**Persist, don't keep ephemeral.** A tiny per-message table is not over-engineering: session
reload/hydration is already a product behavior, and re-running flash on every message on every
session reopen is wasteful and would flicker. Stored labels are hydrated on load; live sends are
classified once.

**No retroactive classification.** Pre-existing history is never back-classified — only messages
that arrive live get a label. Reload shows whatever was previously stored.

## Verified facts (read from repo + installed runtime)

- `@flue/react@1.0.0-beta.7` `useFlueAgent({name,id,history})` returns `AgentSnapshot &
  { sendMessage }`: `messages: UIMessage[]` (each `{id: uuidv7, role, parts[], metadata?}`),
  `status: 'idle'|'connecting'|'submitted'|'streaming'|'error'`, **`historyReady: boolean`**,
  `error`. `sendMessage(text)` returns `Promise<void>` and does **not** expose the optimistic
  message id — so we observe ids via an effect, not via the send call.
- `workflows/session-title.ts` is the template: `defineWorkflow` + inline `defineAgent`
  (`model: deepseek/deepseek-v4-flash`, `thinkingLevel: "minimal"`, `tools: []`), served at
  `POST /api/workflows/<name>?wait=result` via `app.route("/api", flue())`. Invoked
  backend-to-backend from `shared/nav-sessions.ts:requestGeneratedTitle` using loopback fetch
  with `NAV_FLUE_PORT` + `NAV_DESKTOP_TOKEN`.
- `shared/nav-db.ts` owns the sqlite handle on `flue.db` and `ensure*Ready` table creation;
  `nav_sessions` / `nav_projects` live here.
- `app.ts` mounts `/api/sessions*` handlers under `requireDesktopAuth`, **before**
  `app.route("/api", flue())`, and **not** behind `requireCodexProvider`. CORS already allows
  `GET/POST/PATCH/DELETE`.
- `packages/desktop/src/main.tsx` `ConversationMessage` renders user turns as
  `<Message from="user"><MessageContent><ChatMessageParts/></MessageContent></Message>`.
  `Message`/`MessageContent` are shadcn `ai-elements` (**don't hand-edit**); `ConversationMessage`
  is app code (editable). `lib/sessions-client.ts` is the raw-fetch + Bearer-token client pattern.

## Backend (`packages/flue`)

### Workflow — `workflows/request-classifier.ts` (new, clone of session-title)

```ts
// inline agent: model "deepseek/deepseek-v4-flash", thinkingLevel "minimal", tools: []
input:  v.object({ text: v.string(), priorAssistant: v.optional(v.string()) })
output: v.object({ isPlanning: v.boolean(),
                   difficulty: v.picklist(["low","medium","high"]) })
```

Prompt (in `shared/request-classifier.ts`): "You classify a single request to a coding
assistant. `isPlanning` = the request is mainly about planning/decomposing/designing/making a
to-do list, rather than directly executing, answering, or explaining. `difficulty` = low
(trivial/quick) | medium (several steps) | high (complex, large, or ambiguous). Use the prior
assistant turn ONLY to resolve short follow-ups like 'ok do it' / 'same for tests'. Return only
the structured result." Clip `priorAssistant` to ~1–2k chars.

### Persistence — `nav_message_classifications` (new table in `nav-db.ts`)

```
session_id   TEXT NOT NULL
message_id   TEXT NOT NULL      -- the UIMessage.id of the user turn
is_planning  INTEGER NOT NULL   -- 0|1
difficulty   TEXT NOT NULL      -- low|medium|high
created_at   INTEGER NOT NULL
PRIMARY KEY (session_id, message_id)
```

No FK to `nav_sessions` (classify can race the first-send row insert; key independence avoids
ordering bugs). Add an `ensureMessageClassificationsReady()` alongside the existing ensure paths.

### Routes — `shared/request-classifications.ts` (new), registered in `app.ts`

Mount before `flue()`, under `requireDesktopAuth`, **outside** `requireCodexProvider`:

- `POST /api/sessions/:id/classify` — body `{ messageId, text, priorAssistant? }`.
  **Idempotent**: if a row for `(id, messageId)` exists, return it without calling the model.
  Else invoke the `request-classifier` workflow over loopback (mirror `requestGeneratedTitle`),
  upsert, and return `{ isPlanning, difficulty }`. Skip work when `text` is blank
  (attachment-only) — return nothing classifiable.
- `GET /api/sessions/:id/classifications` — returns
  `{ classifications: { messageId, isPlanning, difficulty }[] }` for hydration.
- In `handleDeleteNavSession`: also `DELETE FROM nav_message_classifications WHERE session_id = ?`.

## Frontend (`packages/desktop`)

### Client — extend `lib/sessions-client.ts`

`classifyMessage(sessionId, { messageId, text, priorAssistant })` → POST; `listClassifications(sessionId)` → GET.

### Trigger — effect in `NavChat` (`main.tsx`), `historyReady`-gated

Refs (per active `conversationId`, reset on change):
- `seenUserMessageIdsRef: Set<string>` — message ids that must NOT be classified.
- `inFlightMessageIdsRef: Set<string>` — currently being classified (dedup, incl. React strict-mode double-effect).
- `failedMessageIdsRef: Set<string>` — failures, so we don't retry-loop.
- classification map state: `Map<messageId, {isPlanning,difficulty} | "pending">` → drives render.

Logic:
1. On `conversationId` change: clear all refs + map; `GET /classifications` and seed the map with
   stored rows. Also add every stored `messageId` to `seenUserMessageIdsRef`.
2. When `historyReady` first becomes true: add **every current user `message.id`** to
   `seenUserMessageIdsRef` (hydrated history is thereby never classified).
3. After that, for each user `message.id` not in `seen ∪ inFlight ∪ failed ∪ map`: mark
   `pending`, add to inFlight, `POST /classify` with the user text + clipped prior assistant turn.
   On success → map[id] = result; on failure → add to `failedMessageIdsRef`, drop pending (render
   nothing). Always remove from inFlight. Guard against applying results after a session switch
   (stale `conversationId`).

This never blocks or feeds `sendMessage`; it runs alongside the streaming response.

### Render — `ConversationMessage`, under `MessageContent`

A compact, right-aligned chip row keyed by `message.id`:
- `pending` → muted `Analyzing…`.
- result → `Planning` **or** `Direct` chip + a difficulty badge color-coded
  (low = muted/green, medium = amber, high = red). Both labels always shown (per the user's ask).
- absent/failed → render nothing.

Use existing `components/ui` primitives (e.g. `Badge`) — do not hand-edit `ai-elements`.

## Sequencing

1. Backend: `request-classifier` workflow + `shared/request-classifier.ts` prompt/schema.
2. Backend: `nav_message_classifications` table + `shared/request-classifications.ts`
   (`POST /classify` with idempotency, `GET /classifications`) + register in `app.ts` + delete cleanup.
3. Frontend: `sessions-client` methods.
4. Frontend: `historyReady`-gated trigger effect + classification map state.
5. Frontend: chip render under the user bubble.
6. Lint + format changed files; verify in the real Electron UI (chips appear under live sends,
   not under reloaded history; survive reload; never block the stream).

## Out of scope (v1) — keep it boring

No debounce, no queue/worker service, no retries, no `UIMessage.metadata` mutation, no worktrees,
no backfill of historical messages, no per-project routing of the classifier. The DB primary key
+ the frontend in-flight set are sufficient dedup for v1.

## Risks to verify during implementation

- Loopback workflow invocation path mirrors `requestGeneratedTitle` exactly (env vars, `?wait=result`,
  result shape `json.result`).
- Effect ordering: confirm `historyReady` flips to true **after** the initial `messages` are
  populated, so the seed in step 2 captures all hydrated user ids before any classify fires.
- Confirm the optimistic user message id from `sendMessage` is stable (not replaced by a
  server-assigned id after admission) — if it can change, key the chip off the final id and let
  the seen/inFlight sets dedup the transient.
