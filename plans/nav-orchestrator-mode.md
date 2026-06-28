# Nav Orchestrator Mode

Status: agreed architecture with Pi and Codex on 2026-06-28. Backend v1 implemented in this checkout.

## Goal

Add a project-level toggle that makes Nav run in deterministic orchestrator mode.

When the toggle is on:

1. Nav classifies the current user request before the Nav model starts answering.
2. If the difficulty is `medium` or `high`, Nav delegates the same task to both:
   - `glm`
   - `deepseek-pro`
3. Nav then judges both delegate results and synthesizes the final solution in the real checkout.
4. Nav tracks that the session has entered an orchestrated thread, so follow-up messages are handled with that context.

When the toggle is off, keep the current Nav behavior.

## Hard Constraints

- Do not mutate the user's message text.
- Do not add symbols, control markers, hidden tags, bracket prefixes, or directive tokens to the prompt.
- The visible user message and the message submitted to Flue must remain the same text.
- Use the existing request-classifier model/prompt family; do not add a second follow-up classifier.
- Do not ask the model to write bookkeeping summaries.
- Do not add a model-callable `record_orchestration_progress` tool.
- Do not let server-driven orchestration and model-driven `consult_panel` both run for the same turn.

## Key Decision

Orchestration is a server-side policy, not a user-prompt protocol.

The route for the `nav` agent should inspect the clean request body with `request.clone()`, classify the clean `message`, run the delegate fan-out when required, store the orchestration context, then let Flue continue with the original request body untouched.

Nav receives the orchestration context through normal system instructions built from server state, not through markers inside the user message.

## Existing Pieces To Reuse

- `packages/flue/.flue/agents/nav.ts`
  - User-facing Nav agent.
  - Already resolves the session project and git context.
  - Already decides which tools Nav receives.
- `packages/flue/.flue/shared/delegation.ts`
  - Existing loopback delegate calls.
  - Existing `consult_panel` logic already runs agents in parallel.
- `packages/flue/.flue/agents/glm.ts`
  - GLM delegate.
- `packages/flue/.flue/agents/deepseek-pro.ts`
  - DeepSeek V4 Pro delegate.
- `packages/flue/.flue/shared/request-classifier.ts`
  - Existing classification schema and prompt helpers.
- `packages/flue/.flue/shared/request-classifications.ts`
  - Existing loopback workflow call pattern for classifier.
- `packages/flue/.flue/shared/worktrees.ts`
  - Existing per-delegation worktree snapshots.
- `packages/flue/.flue/shared/nav-projects.ts`
  - Existing project settings shape.
- `packages/desktop/src/components/app-sidebar.tsx`
  - Existing project actions menu and `Auto-approve edits` checkbox pattern.

## Architecture

```
Desktop sends clean user message
  -> POST /api/agents/nav/:sessionId
  -> nav route clones request body and reads message
  -> if project orchestrator toggle is off:
       continue current behavior
  -> if toggle is on:
       classify clean message with request-classifier workflow
       write nav_orchestrator_turns row
       if difficulty is medium or high:
         set/keep session orchestrator state active
         run glm and deepseek-pro in parallel
         store delegate answers and worktree paths
       if difficulty is low:
         do not fan out
         keep active state if already active
  -> route stores turn context in SQLite keyed by session id
  -> Flue runs Nav on the original unmodified request
  -> Nav instructions read latest server context from SQLite
  -> Nav synthesizes final answer/change in the real checkout
```

## Project Toggle

Add a project setting named `orchestratorEnabled`.

### Database

Edit `packages/flue/.flue/shared/nav-db.ts`.

Add a project column:

```sql
orchestrator_enabled INTEGER NOT NULL DEFAULT 0
```

Add it through the existing `hasColumn` migration loop in `ensureNavProjectTable`.

### Backend Project Types

Edit `packages/flue/.flue/shared/nav-projects.ts`.

Add the field to:

- `NavProjectRow`
- `SessionProjectRow`
- `NavProjectSummary`
- `ResolvedSessionProject`
- project selection SQL
- `serializeProject`
- `resolveSessionProject`
- `handleUpdateNavProject`

Validation:

```ts
if ("orchestratorEnabled" in body) {
  if (typeof body.orchestratorEnabled !== "boolean") {
    return c.json({ error: "invalid_orchestrator_enabled" }, 400);
  }
  sets.push("orchestrator_enabled = ?");
  values.push(body.orchestratorEnabled ? 1 : 0);
}
```

### Frontend Types

Edit `packages/desktop/src/lib/projects-client.ts`.

Add:

```ts
orchestratorEnabled: boolean;
```

to `NavProject`.

Add:

```ts
orchestratorEnabled?: boolean;
```

to `ProjectUpdate`.

### Frontend Toggle

Edit `packages/desktop/src/components/app-sidebar.tsx`.

Add a `DropdownMenuCheckboxItem` near `Auto-approve edits`:

```tsx
<DropdownMenuCheckboxItem
  checked={project.orchestratorEnabled}
  onCheckedChange={(checked) => {
    void updateProject(project.id, {
      orchestratorEnabled: checked === true,
    });
  }}
>
  Orchestrator mode
</DropdownMenuCheckboxItem>
```

## Orchestrator State

We need lightweight state so Nav knows when a session is inside an orchestrated thread.

This state is not model-written and does not contain a model-authored summary.

### `nav_orchestrator_state`

One row per Nav session.

```sql
CREATE TABLE IF NOT EXISTS nav_orchestrator_state (
  session_id TEXT PRIMARY KEY,
  project_id TEXT,
  active INTEGER NOT NULL DEFAULT 0,
  thread_id TEXT,
  started_at INTEGER,
  updated_at INTEGER NOT NULL,
  cleared_at INTEGER
);
```

Rules:

- A thread becomes active on the first `medium` or `high` turn while the toggle is on.
- Active state stays active for that session.
- Active state clears on:
  - new session
  - session delete
  - project toggle off
  - future explicit reset action
- V1 does not use an automatic "new task vs same task" classifier.

### `nav_orchestrator_turns`

One row per user request processed while the toggle is on.

```sql
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
);

CREATE INDEX IF NOT EXISTS nav_orchestrator_turns_session_idx
  ON nav_orchestrator_turns (session_id, created_at);
```

`mode`:

- `direct`
- `panel`

`status`:

- `pending`
- `complete`
- `partial`
- `failed`

### `nav_orchestrator_delegate_results`

One row per delegate attempt in a panel turn.

```sql
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
);

CREATE INDEX IF NOT EXISTS nav_orchestrator_delegate_results_turn_idx
  ON nav_orchestrator_delegate_results (turn_id);
```

`agent` is one of:

- `glm`
- `deepseek-pro`

`status`:

- `complete`
- `failed`
- `timeout`

## Worktree Decision

Pi recommended considering persistent delegate worktrees across an orchestrated thread. After checking the current helper, v1 should keep fresh physical worktrees per panel turn.

Reason:

- Existing `createAgentWorktree` snapshots Nav's current checkout into the delegate worktree.
- Nav's final synthesis happens in the real checkout, not in delegate worktrees.
- Reusing a prior delegate worktree can leave delegates looking at stale files that do not include Nav's synthesized final change.

V1 rule:

- Each `medium` or `high` panel turn creates fresh delegate session ids and fresh worktrees.
- The source of truth is always Nav's real checkout at the moment of fan-out.
- Follow-up continuity comes from the Nav session transcript and the orchestrator state/turn log.

Future optimization:

- Reuse delegate worktrees only if we also implement a safe refresh/rebase/reset step from Nav's real checkout before each follow-up fan-out.

## Server-Side Orchestrator Module

Add `packages/flue/.flue/shared/orchestrator.ts`.

Responsibilities:

- Read the project toggle and active state.
- Classify a clean request when the toggle is on.
- Create and update orchestrator turn rows.
- Run delegate fan-out for `medium` and `high`.
- Store delegate results.
- Expose latest persisted turn context for `agents/nav.ts`.
- Clear state on toggle off/session delete.

Suggested exported functions:

```ts
type OrchestratorTurnContext = {
  active: boolean;
  difficulty: "low" | "medium" | "high" | null;
  isPlanning: boolean;
  mode: "direct" | "panel";
  status: "complete" | "partial" | "failed";
  turnId: string;
  threadId: string | null;
  delegateResults: {
    agent: "glm" | "deepseek-pro";
    answer: string;
    status: "complete" | "failed" | "timeout";
    worktree: string | null;
    error?: string;
  }[];
};

async function prepareOrchestratorTurn(input: {
  sessionId: string;
  project: ResolvedSessionProject;
  git: GitContext;
  message: string;
  signal?: AbortSignal;
}): Promise<OrchestratorTurnContext | null>;

function getLatestOrchestratorContext(
  sessionId: string,
): OrchestratorTurnContext | null;

function clearOrchestratorStateForSession(sessionId: string): void;
```

`prepareOrchestratorTurn` returns `null` when:

- project toggle is off
- project is not git-backed

Blank text and image-bearing requests are logged as direct turns with `difficulty = null`, so Nav can still see that orchestrator mode was considered but no panel ran.

## Classification

Only classify when the project toggle is on.

Use the existing request-classifier workflow/model path:

- `deepseek/deepseek-v4-flash`
- `thinkingLevel: "minimal"`
- structured result:
  - `isPlanning`
  - `difficulty`

Do not add a separate follow-up classifier.

Classification failure policy:

- Do not block Nav from answering.
- Log a `failed` or `direct` orchestrator turn with the classification error.
- Pass system context to Nav that classification failed and no delegates were run.

## Deterministic Fan-Out

If toggle is on and classifier returns:

- `low`
  - Do not run delegates.
  - If the session is already in an orchestrated thread, keep `active = true`.
  - Nav handles the follow-up directly with orchestrated-thread context.
- `medium`
  - Run `glm` and `deepseek-pro` in parallel.
  - Mark session state active.
- `high`
  - Run `glm` and `deepseek-pro` in parallel.
  - Mark session state active.

No domain routing in v1. Medium and high both go to both delegates.

## Delegate Failure Policy

Run both delegate wrappers concurrently and convert each delegate outcome into a stored result, so one delegate failure cannot reject the whole panel before the other result is captured.

If both delegates succeed:

- turn `status = complete`
- Nav receives both delegate outputs/worktrees.

If one delegate fails or times out:

- turn `status = partial`
- Nav receives the successful delegate result and the failed delegate error.
- Nav continues synthesis with what exists.

If both delegates fail:

- turn `status = failed`
- Nav receives the error context and handles the request directly.

Do not let one flaky delegate stall the whole Nav turn indefinitely.

Add a per-delegate timeout constant in `shared/orchestrator.ts`. Keep it conservative and configurable by env:

```ts
const DEFAULT_ORCHESTRATOR_DELEGATE_TIMEOUT_MS = 15 * 60 * 1000;
```

## Nav Agent Route

Edit `packages/flue/.flue/agents/nav.ts`.

The `route` handler becomes the pre-turn policy point.

Pseudo-flow:

```ts
export const route: AgentRouteHandler = async (c, next) => {
  if (c.req.method !== "POST") {
    await next();
    return;
  }

  const sessionId = c.req.param("id");
  const body = await c.req.raw.clone().json();
  const message = typeof body.message === "string" ? body.message : "";

  const project = resolveSessionProject(sessionId);
  const git = resolveGitContext(project.path);

  if (project.orchestratorEnabled && git.ok) {
    await prepareOrchestratorTurn({
      sessionId,
      project,
      git: git.context,
      message,
      signal: c.req.raw.signal,
    });
  }

  await next();
};
```

Important:

- Use `c.req.raw.clone()` so the original request body remains readable by Flue.
- Never rewrite the body.
- Never add markers to `message`.

## Nav Agent Instructions

Still in `agents/nav.ts`, `defineAgent` reads the latest persisted orchestrator context:

```ts
const orchestrator = getLatestOrchestratorContext(ctx.id);
```

When `orchestratorEnabled` is false:

- Preserve the current Nav behavior and current consult tools.

When `orchestratorEnabled` is true:

- Do not include `consult` or `consult_panel` tools in Nav's tool list.
- Server-side orchestration owns delegation for this project.
- Nav still has its normal local sandbox capabilities and can inspect files/worktrees with shell commands.

Instruction cases:

### Toggle On, Direct Turn

Append natural-language context like:

> Orchestrator mode is enabled. This request was classified as low difficulty, so no delegate panel was run. If this is a follow-up in an active orchestrated thread, keep that prior thread context in mind and handle this turn directly.

### Toggle On, Panel Turn With Both Results

Append natural-language context like:

> Orchestrator mode is enabled. This request was classified as medium/high difficulty. The server already ran the same task through glm and deepseek-pro. Inspect each returned worktree with git diff, judge both solutions, take the useful parts from each, and write the final synthesized result in the active project checkout. Do not run another delegate panel for this turn.

Include:

- turn id
- difficulty
- each delegate name
- answer excerpt or full answer
- worktree path
- delegate status

### Toggle On, Partial Results

Append context like:

> The delegate panel partially completed. Use the successful result(s), account for the failed delegate(s), and continue the final synthesis without waiting for another delegation.

### Toggle On, Both Delegates Failed

Append context like:

> The delegate panel failed. No usable delegate output is available. Continue directly in the active checkout.

## Consult Tool Coexistence

Avoid double delegation.

Rules:

- Toggle off:
  - current behavior can remain: Nav gets `consult` and `consult_panel` when git context is valid.
- Toggle on:
  - Nav does not get `consult` or `consult_panel`.
  - The server route performs deterministic fan-out.

This keeps the model from running an extra panel after the server already ran one.

## Follow-Up Handling

No follow-up classifier.

Every turn while the toggle is on is classified with the same normal request classifier.

Rules:

- If no active thread and current difficulty is `low`:
  - direct turn
  - state remains inactive
- If no active thread and current difficulty is `medium` or `high`:
  - panel turn
  - state becomes active
- If active thread and current difficulty is `low`:
  - direct turn
  - state remains active
  - Nav is told this is a low-difficulty turn inside an active orchestrated thread
- If active thread and current difficulty is `medium` or `high`:
  - panel turn
  - state remains active

Thread boundary for v1:

- Starts on first medium/high turn while toggle is on.
- Stays active for the session.
- Clears on session delete, new session, toggle off, or a future explicit reset action.

Do not try to infer "new task" automatically in v1.

## API Additions

No custom send endpoint in v1.

Use the existing Flue agent route and keep `useFlueAgent.sendMessage` unchanged.

Optional read endpoints for UI/debugging:

```http
GET /api/sessions/:id/orchestrator
```

Response:

```ts
{
  state: {
    active: boolean;
    threadId: string | null;
    startedAt: number | null;
    updatedAt: number | null;
  };
  turns: {
    id: string;
    difficulty: "low" | "medium" | "high" | null;
    mode: "direct" | "panel";
    status: "pending" | "complete" | "partial" | "failed";
    createdAt: number;
    completedAt: number | null;
    delegates: {
      agent: "glm" | "deepseek-pro";
      status: "complete" | "failed" | "timeout";
      worktree: string | null;
      error?: string;
    }[];
  }[];
}
```

This endpoint is not required for the first backend slice, but it is useful for showing an "Orchestrator active" UI pill later.

## Session Delete And Toggle Off Cleanup

Edit `packages/flue/.flue/shared/nav-sessions.ts`.

When deleting a session, also delete:

- `nav_orchestrator_state`
- `nav_orchestrator_turns`
- `nav_orchestrator_delegate_results`

Edit `packages/flue/.flue/shared/nav-projects.ts`.

When `orchestratorEnabled` changes from true to false:

- Clear active orchestrator state for sessions in that project, or mark it inactive.
- Do not delete historical turn logs.

## Implementation Slices

### Slice 1: Project Toggle

Files:

- `packages/flue/.flue/shared/nav-db.ts`
- `packages/flue/.flue/shared/nav-projects.ts`
- `packages/desktop/src/lib/projects-client.ts`
- `packages/desktop/src/components/app-sidebar.tsx`

Acceptance:

- Toggle appears in project menu.
- Toggle persists.
- Toggle hydrates on app restart.
- Invalid update payloads are rejected.

### Slice 2: Orchestrator Tables And State Helpers

Files:

- `packages/flue/.flue/shared/nav-db.ts`
- `packages/flue/.flue/shared/orchestrator.ts`
- `packages/flue/.flue/shared/nav-sessions.ts`

Acceptance:

- Tables are created.
- State can become active.
- State can clear.
- Turn logs and delegate result logs insert correctly.
- Session delete removes state/log rows.

### Slice 3: Classify In Nav Route

Files:

- `packages/flue/.flue/agents/nav.ts`
- `packages/flue/.flue/shared/request-classifications.ts`
- `packages/flue/.flue/shared/orchestrator.ts`

Acceptance:

- Toggle off does not classify.
- Toggle on classifies before Nav model instructions are built.
- Original request body is not mutated.
- Classification failure degrades to direct handling.

### Slice 4: Server-Driven Panel Fan-Out

Files:

- `packages/flue/.flue/shared/delegation.ts`
- `packages/flue/.flue/shared/orchestrator.ts`
- `packages/flue/.flue/agents/nav.ts`

Acceptance:

- Medium/high calls both `glm` and `deepseek-pro`.
- Calls run concurrently.
- Results are logged.
- Partial failures are logged and passed to Nav.
- Both-failed case still lets Nav answer directly.

### Slice 5: Nav Instruction Context And Tool Gating

Files:

- `packages/flue/.flue/agents/nav.ts`

Acceptance:

- Toggle off preserves current consult-tool behavior.
- Toggle on hides `consult` and `consult_panel`.
- Toggle on direct turn tells Nav no panel ran.
- Toggle on panel turn tells Nav delegates already ran and gives worktree paths.
- Nav is explicitly told to synthesize final edits in the active checkout.

### Slice 6: Optional UI State

Files:

- `packages/flue/.flue/shared/orchestrator.ts`
- `packages/flue/.flue/app.ts`
- `packages/desktop/src/lib/sessions-client.ts`
- `packages/desktop/src/main.tsx`

Acceptance:

- UI can show an "Orchestrator active" indicator.
- UI can show last panel status.
- This slice can be deferred if backend behavior is enough for v1.

## Tests

Add focused tests to `tests/flue_m2_acceptance.test.js` or create `tests/nav_orchestrator.test.js`.

### Project Toggle Tests

- Default `orchestratorEnabled` is false.
- PATCH accepts `orchestratorEnabled: true`.
- PATCH accepts `orchestratorEnabled: false`.
- PATCH rejects non-boolean values.
- `resolveSessionProject` includes the flag.

### Route Classification Tests

- Toggle off: no classifier call.
- Toggle on: classifier called with clean user text.
- Original body remains unchanged after route pre-read.
- Blank text skips classification.
- Classification failure creates direct/failure context and does not block Nav.

### Fan-Out Tests

- `low` returns direct mode and does not call delegates.
- `medium` calls exactly `glm` and `deepseek-pro`.
- `high` calls exactly `glm` and `deepseek-pro`.
- Delegate calls are launched in parallel.
- One delegate failure yields `partial`.
- Two delegate failures yield `failed`.
- Worktree paths are recorded for successful delegates.

### State Tests

- First medium/high turn activates state.
- Low turn before activation does not activate state.
- Low turn after activation keeps state active.
- Toggle off clears active state.
- Session delete removes orchestrator state and logs.

### Tool Gating Tests

- Toggle off and git ok: Nav includes current consult tools.
- Toggle on and git ok: Nav does not include consult tools.
- Toggle on and panel context exists: instructions include delegate worktree paths.
- Toggle on and direct context exists: instructions state no panel ran.

### Regression Tests

- Existing `consult_panel` behavior remains available when toggle is off.
- Existing request classification chips still work.
- Existing session title generation still works.
- Existing project update fields still work.

## Verification Commands

Before finishing implementation:

```sh
pnpm run format
pnpm run lint
pnpm run test
pnpm --filter @nav/desktop build
pnpm --filter @nav/flue typecheck
pnpm --filter @nav/flue build
git diff --check
```

## Open Questions

These are deliberately not part of v1:

1. Should there be a visible "Reset orchestrator thread" button?
2. Should medium eventually route to one delegate instead of both for latency/cost?
3. Should delegate physical worktrees be reused after implementing a safe refresh from Nav's checkout?
4. Should the UI render the per-turn delegate log under each user message?

## Final Agreed Shape

- Clean user prompt.
- No prompt symbols or hidden directive markers.
- Toggle gates all added latency and behavior.
- Server-side classification before Nav answers.
- Deterministic dual fan-out for `medium` and `high`.
- Lightweight state tracks whether the session is in an orchestrated thread.
- Append-only turn/delegate logs provide observability.
- No second follow-up classifier.
- No model-written progress summary.
- No extra model-callable bookkeeping tool.
