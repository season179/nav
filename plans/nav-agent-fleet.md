# Plan: Nav agent fleet — independent top-level agents + loopback delegation (Architecture B)

**Status:** ✅ **Converged** — debated to agreement between Claude and Codex over two
adversarial rounds; every load-bearing claim verified against the *installed* source. This is
the **single source of truth**: the verified model/provider catalog facts and the full
provider/profile source are folded into **§10**, so the old `plans/nav-glm-agent.md` and
`plans/nav-deepseek-agent.md` have been **removed**. glm currently ships as a *subagent*; §9
migrates it to Architecture B.

Verified against the installed source of `@flue/runtime@1.0.0-beta.7` and
`@earendil-works/pi-ai@0.79.10`. Code blocks are implementation sketches, not final code.

---

## 1. Why B (and what it costs)

The user wants this concrete workflow:

> Nav hands the **same task** to glm *and* deepseek at once. Each returns a solution;
> neither is strictly better. Nav takes the good parts of each and synthesizes one final
> best solution.

For that, glm and deepseek must edit **without colliding** → each needs **its own working
copy of the repo**. That requirement collides with how Flue subagents work, and the
collision is what forces Architecture B. The load-bearing facts (all verified):

| # | Fact | Source |
|---|------|--------|
| B1 | A subagent **profile** (`defineAgentProfile`) has **no `sandbox` and no `cwd` field** — it cannot own a workspace. | `@flue/runtime` `action-*.d.mts:314` (`interface AgentProfile`) |
| B2 | Only a **top-level** `defineAgent` config (`AgentRuntimeConfig`) has `sandbox?: SandboxFactory` and `cwd?: string`. | same, `:345` |
| B3 | A `task()` delegation (`TaskOptions`) adds only `agent` + `cwd` — it can override the working dir but **not** the sandbox factory, and only reaches **declared subagents**. | same, `:693` |
| B4 | **There is no exported in-process "run another top-level agent and await its reply" primitive.** `createAgent` is just a deprecated alias of `defineAgent`; `dispatch()` returns only a `DispatchReceipt` (fire-and-forget, no reply); `FlueSession` has no top-level-agent selector. The simple path is a **loopback HTTP** call: `POST /api/agents/:name/:id?wait=result` → `200 { result: { text, usage, model }, … }`. | `handle-agent-*.mjs:141` (`createAgent`); `api/agent-api` (`dispatch`→`DispatchReceipt`, `FlueSession`); `api/routing-api` (`?wait=result`) |
| B5 | An agent is HTTP-exposed when the runtime sees **`agent.route !== undefined`** (or the dev-only `temporaryLocalExposure`). In practice: **export `route`** from the agent module. | `flue-app-*.mjs:759` (`registeredAgentsForTransport`); `index.mjs:219` (`listAgents`) |
| B6 | `requireDesktopAuth` checks **only** `Authorization: Bearer == NAV_DESKTOP_TOKEN` (no server-side Origin check), so an in-process loopback `fetch` with that bearer is accepted. | `packages/flue/.flue/app.ts:28–46` |

**The cost of B** (accepted): glm/deepseek become first-class agents, but Nav reaches them
over a **loopback HTTP call** (raw `fetch`, no `@flue/sdk` needed) instead of the built-in
`task` tool, and the fleet needs **git-worktree workspace plumbing**. Context isolation is
unchanged — each agent runs in its own session; Nav only ever receives the returned text.

---

## 2. Target architecture

Four independent, auto-discovered top-level agents under `agents/`. Each **delegation** runs
in its **own git worktree** (one per agent *instance*, not per agent name — see §5 Step 2):

| Level | Agent (`agents/*.ts`) | Model | Workspace |
|-------|-----------------------|-------|-----------|
| L5 | **nav** (lead, the only user-facing agent) | `openai-codex/gpt-5.5` | `local({ cwd: repoRoot })` — the real checkout |
| L3 | **glm** | `zai/glm-5.2` | a fresh worktree per delegation |
| L2 | **deepseek-pro** | `deepseek/deepseek-v4-pro` | a fresh worktree per delegation |
| L1 | **deepseek-flash** | `deepseek/deepseek-v4-flash` | a fresh worktree per delegation |

```
        user ── chat ──▶ nav (L5, real checkout)
                          │  consult / consult_panel  (loopback HTTP, Bearer token)
            ┌─────────────┼───────────────────────────┐
            ▼             ▼                             ▼
   glm (own worktree) deepseek-pro (own wt)   deepseek-flash (own wt)
            └── each returns { answer, worktree }; edits land in ITS worktree ──┘
                          │
        nav reads `git -C <worktree> diff` for each, synthesizes the
        final result in its OWN checkout.
```

- **No subagents.** Nav declares no `subagents`; the `task` tool is gone. Delegation is the
  loopback **`consult` / `consult_panel`** tools (§5 Step 4).
- **Profiles are reused, not discarded.** `glmProfile` / `deepseekProProfile` /
  `deepseekFlashProfile` stay in `shared/` and become the **baseline `profile`** of each
  top-level agent (`defineAgent((ctx) => ({ profile, sandbox }))`). Model/instructions/
  thinkingLevel all come from the profile; the agent file only adds the workspace + route.
- **No recursion, no secret leak.** Only Nav gets the consult tools; delegates can't re-enter
  the fleet. Delegate `local()` sandboxes get the **default tight env** — never
  `{ ...process.env }`, or `NAV_DESKTOP_TOKEN`/`NAV_FLUE_PORT`/API keys would leak into a
  model-directed shell (B-review #9).

---

## 3. Files at a glance

| Action | Path | Purpose |
|--------|------|---------|
| **keep** | `shared/glm.ts`, `shared/zai-provider.ts` | glm profile + zai provider (already shipped). Unchanged. |
| **new** | `shared/deepseek.ts`, `shared/deepseek-provider.ts` | deepseek profiles + provider (do **not** exist yet — source in §10). |
| **new** | `shared/worktrees.ts` | `createAgentWorktree(agent, instanceId)` / `pruneAgentWorktrees()`. |
| **new** | `shared/delegation.ts` | Loopback `consult` / `consult_panel` tools. |
| **new** | `agents/glm.ts`, `agents/deepseek-pro.ts`, `agents/deepseek-flash.ts` | Top-level agents: `profile` + per-instance worktree sandbox + `route`. |
| **edit** | `agents/nav.ts` | **Remove** `subagents`; **add** `tools: [consult, consultPanel]`; rewrite delegation instructions. |
| **edit** | `app.ts` | Register deepseek provider; `pruneAgentWorktrees()` at boot; scope `requireCodexProvider` to nav only. |
| **edit** | `packages/desktop/src/main-process/flue-server.ts` | Pass the chosen Flue port (`NAV_FLUE_PORT`) into the server env so the loopback can find it. |

`@flue/sdk` is **not** required — the loopback uses plain `fetch`.

---

## 4. The compete-and-merge flow (the user's scenario, concretely)

1. User asks Nav for a non-trivial change.
2. Nav calls **`consult_panel({ agents: ["glm", "deepseek-pro"], task: "<spec incl. 'implement the change in your working copy'>" })`**.
3. The tool fires two loopback prompts **in parallel**; each runs as a fresh instance in its
   **own worktree** (snapshotted from Nav's current tree). Each returns `{ answer, worktree }`.
4. Nav reads the *actual code* each produced with its own shell tool, using the returned
   path: `git -C <worktree> diff`.
5. Nav compares both (text rationale + real diffs), takes the best of each, and **writes the
   merged solution in its own checkout** (`repoRoot`), where the user sees it.

---

## 5. Step-by-step

### Step 1 — Profiles stay as-is (reused as baselines)

`shared/glm.ts` (exists) and `shared/deepseek.ts` (new) export sandbox-less profiles with
model + thinkingLevel + instructions. **Full source + verified catalog facts are in §10.**
The existing "prefer read-only … unless the delegating agent explicitly asks you to make
changes" line stays — Nav's consult prompt **will** explicitly authorize edits in the
delegate's own worktree.

### Step 2 — Worktree helper: `shared/worktrees.ts`

Two fixes from review baked in: **(a)** one worktree per *instance* (not per agent name), so
two concurrent same-agent delegations never share a tree; **(b)** worktrees live **outside**
the checkout, scoped by a repo hash, so the `flue dev` watcher doesn't reload on delegate
edits and neighboring Nav checkouts don't delete each other's delegates; **(c)** each is based
on a synthetic **snapshot of Nav's current working tree** (not just `HEAD`) so delegates see
in-progress tracked changes, deletions, and non-ignored untracked files.

```ts
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { getWorkspaceRoot } from "./codex.js";

function worktreeRoot(repoRoot: string): string {
  const repoHash = createHash("sha256").update(repoRoot).digest("hex").slice(0, 12);
  return path.join(os.tmpdir(), "nav-worktrees", repoHash); // OUTSIDE the repo
}

export function agentWorktreePath(agent: string, instanceId: string): string {
  return path.join(worktreeRoot(getWorkspaceRoot()), agent, instanceId);
}

function createWorkspaceSnapshot(repoRoot: string): string {
  const tempDir = mkdtempSync(path.join(os.tmpdir(), "nav-snapshot-index-"));
  const env = {
    ...process.env,
    GIT_AUTHOR_EMAIL: "nav@example.invalid",
    GIT_AUTHOR_NAME: "Nav Delegation",
    GIT_COMMITTER_EMAIL: "nav@example.invalid",
    GIT_COMMITTER_NAME: "Nav Delegation",
    GIT_INDEX_FILE: path.join(tempDir, "index"),
  };
  try {
    execFileSync("git", ["-C", repoRoot, "read-tree", "HEAD"], { env, stdio: "ignore" });
    execFileSync("git", ["-C", repoRoot, "add", "-A"], { env, stdio: "ignore" });
    const tree = execFileSync("git", ["-C", repoRoot, "write-tree"], {
      encoding: "utf8",
      env,
    }).trim();
    return execFileSync(
      "git",
      ["-C", repoRoot, "commit-tree", tree, "-p", "HEAD", "-m", "nav delegation snapshot"],
      { encoding: "utf8", env },
    ).trim();
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

// The snapshot uses a temporary Git index, so the user's real index and working tree are not
// touched. The worktree is detached at that synthetic commit, so no branch churn.
export function createAgentWorktree(agent: string, instanceId: string): string {
  const repoRoot = getWorkspaceRoot();
  const wt = agentWorktreePath(agent, instanceId);
  if (!existsSync(wt)) {
    const snapshot = createWorkspaceSnapshot(repoRoot);
    mkdirSync(path.dirname(wt), { recursive: true });
    execFileSync("git", ["-C", repoRoot, "worktree", "add", "--detach", wt, snapshot], {
      stdio: "ignore",
    });
  }
  return wt;
}

// Boot-time reclaim. NOTE: `git worktree prune` only drops registry entries whose working
// dirs are ALREADY gone — it does NOT delete live worktree dirs. To reclaim disk you must
// remove the dirs first, THEN prune. Safe to rm only at boot, when no delegation is in flight
// (a UUID-keyed dir left from a previous run is always stale). Omit the rm to keep them.
export function pruneAgentWorktrees(): void {
  const repoRoot = getWorkspaceRoot();
  rmSync(worktreeRoot(repoRoot), { recursive: true, force: true });
  execFileSync("git", ["-C", repoRoot, "worktree", "prune"], { stdio: "ignore" });
}
```

### Step 3 — Top-level delegate agents (one file each)

```ts
// agents/glm.ts
import { type AgentRouteHandler, defineAgent } from "@flue/runtime";
import { local } from "@flue/runtime/node";
import { glmProfile } from "../shared/glm.js";
import { createAgentWorktree } from "../shared/worktrees.js";

export const description =
  "glm (GLM-5.2) — senior full-stack engineer. Works in its own per-delegation checkout.";

// Pass-through; app.ts middleware enforces the bearer-token auth. (B5: exporting `route` is
// what HTTP-exposes the agent.)
export const route: AgentRouteHandler = async (_c, next) => {
  await next();
};

export default defineAgent((ctx) => {
  const cwd = createAgentWorktree("glm", ctx.id); // ctx.id = the agent INSTANCE id
  return { profile: glmProfile, sandbox: local({ cwd }) };
  // Do NOT add `local({ cwd, env: { ...process.env } })` — it would leak NAV_DESKTOP_TOKEN /
  // API keys into a model-directed shell. If a command needs env, pass a tight allowlist only.
});
```

`agents/deepseek-pro.ts` / `agents/deepseek-flash.ts` are identical except the imported
profile, the `createAgentWorktree("…")` name, and the description. **Model/instructions/
thinkingLevel all come from the profile — do not restate them here.**

### Step 4 — Loopback delegation tools: `shared/delegation.ts`

```ts
import { randomUUID } from "node:crypto";
import { defineTool } from "@flue/runtime";
import * as v from "valibot";
import { agentWorktreePath } from "./worktrees.js";

const FLEET = ["glm", "deepseek-pro", "deepseek-flash"] as const;

async function consultAgent(agent: string, message: string, signal?: AbortSignal) {
  const port = process.env.NAV_FLUE_PORT;
  const token = process.env.NAV_DESKTOP_TOKEN;
  if (!port || !token) throw new Error("NAV_FLUE_PORT / NAV_DESKTOP_TOKEN not set");
  const id = randomUUID(); // fresh instance => fresh context + fresh worktree per delegation
  const res = await fetch(`http://127.0.0.1:${port}/api/agents/${agent}/${id}?wait=result`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify({ message }),
    signal,
  });
  if (!res.ok) throw new Error(`consult ${agent} failed: ${res.status} ${await res.text()}`);
  const json = (await res.json()) as { result?: { text?: string } };
  return { agent, answer: json.result?.text ?? "", worktree: agentWorktreePath(agent, id) };
}

export const consult = defineTool({
  name: "consult",
  description:
    "Delegate a task to ONE engineer (glm | deepseek-pro | deepseek-flash). It works in its own checkout and returns its solution plus the `worktree` path. Inspect its real changes with `git -C <worktree> diff`.",
  input: v.object({ agent: v.picklist(FLEET), task: v.string() }),
  output: v.object({ agent: v.string(), answer: v.string(), worktree: v.string() }),
  async run({ input, signal }) {
    return consultAgent(input.agent, input.task, signal);
  },
});

export const consultPanel = defineTool({
  name: "consult_panel",
  description:
    "Delegate the SAME task to several engineers in PARALLEL; returns each one's { answer, worktree } so you can compare the real diffs and synthesize the best final answer.",
  input: v.object({ agents: v.array(v.picklist(FLEET)), task: v.string() }),
  output: v.object({
    results: v.array(v.object({ agent: v.string(), answer: v.string(), worktree: v.string() })),
  }),
  async run({ input, signal }) {
    if (new Set(input.agents).size !== input.agents.length) {
      throw new Error("consult_panel: duplicate agents are not allowed"); // review #4
    }
    const results = await Promise.all(
      input.agents.map((a) => consultAgent(a, input.task, signal)),
    );
    return { results };
  },
});
```

### Step 5 — Migrate Nav: `agents/nav.ts`

```diff
-import { glmProfile } from "../shared/glm.js";
+import { consult, consultPanel } from "../shared/delegation.js";
@@
-      "You are the lead. When a sub-problem is hard, ambiguous, or high-judgment, delegate it to the `glm` subagent, a senior full-stack engineer, and build on its findings. Handle routine work yourself, and do not delegate trivial lookups or image-based tasks.",
+      "You are the lead, coordinating a team of engineers who each work in their own separate checkout of this repo. Use `consult` to delegate one task to one engineer, or `consult_panel` to give the SAME task to several at once and compare. Route by difficulty, not domain: hard/ambiguous/high-judgment → glm (senior); well-scoped mechanical work → deepseek-pro (junior); small trivial fully-specified tasks → deepseek-flash (fresh grad). Each result includes a `worktree` path — read its real changes with `git -C <worktree> diff`, take the best parts of each, and write the final result in your own checkout. Never delegate image-based tasks (all delegates are text-only).",
     ].join(" "),
     model: "openai-codex/gpt-5.5",
     sandbox: local({ cwd: repoRoot }),
-    subagents: [glmProfile],
+    tools: [consult, consultPanel],
     thinkingLevel: resolveThinkingLevel(),
   };
 });
```

### Step 6 — Boot wiring: `app.ts`

```diff
+import { ensureDeepseekProvider } from "./shared/deepseek-provider.js";
+import { pruneAgentWorktrees } from "./shared/worktrees.js";
@@
 ensureZaiProvider();
+ensureDeepseekProvider();
+try {
+  pruneAgentWorktrees(); // best-effort reap of stale per-instance worktrees
+} catch (error) {
+  console.warn(
+    "[nav] Failed to prune stale agent worktrees:",
+    error instanceof Error ? error.message : error,
+  );
+}
```

Worktrees are created **lazily per delegation** (in each agent's initializer), not at boot.

**Scope `requireCodexProvider` to nav only.** Today it gates `/api/agents/*` (app.ts:115), so
a Codex outage would needlessly 503 glm/deepseek (which don't use Codex). Change to gate only
the nav route, e.g. `app.use("/api/agents/nav/*", requireCodexProvider)`. *(Confirm the exact
mounted path; if per-agent scoping isn't clean, leaving it is acceptable since Nav always
needs Codex anyway.)*

### Step 7 — Make the port reachable: `packages/desktop/src/main-process/flue-server.ts`

The child env currently passes **only** `NAV_DESKTOP_ORIGIN` + `NAV_DESKTOP_TOKEN`
(verified flue-server.ts:97–103). Add the Flue port the desktop launches the server on:

```diff
 env: {
   ...process.env,
   NAV_DESKTOP_ORIGIN: ...,
   NAV_DESKTOP_TOKEN: this.#token,
+  NAV_FLUE_PORT: String(port), // wire the actual chosen Flue server port
 },
```

### Step 8 — Keys & env

`ZAI_API_KEY` (glm), `DEEPSEEK_API_KEY` (both deepseek), `NAV_DESKTOP_TOKEN` (set by desktop),
and `NAV_FLUE_PORT` (Step 7) must be present in the Flue server process. **API keys are used by
the provider layer only — they must NOT be forwarded into delegate sandboxes.**

---

## 6. Decisions locked (Claude ↔ Codex converged)

| Decision | Outcome | Rationale |
|----------|---------|-----------|
| Topology | 4 independent top-level agents; **no subagents** | B1–B4: own sandbox factory requires top-level agents; no in-process await-primitive exists. |
| Cross-agent calls | Loopback `fetch` → `POST /api/agents/:name/:id?wait=result` with Bearer token | B4/B6: only path that runs the target in its own sandbox and returns a reply; no new dep. |
| Workspaces | **One git worktree per delegation instance**, **outside** the checkout and scoped by repo hash, based on a synthetic temporary-index **snapshot** of Nav's current tree | review #4/#5/#7 plus implementation review: avoids concurrent-collision, watcher noise, stale-HEAD, and cross-checkout pruning; delegates see in-progress code, including non-ignored untracked files. |
| Toolchain in worktrees | v1: delegates **edit + statically inspect only**; Nav runs tests after merge. **No `node_modules` symlink** | review #6: symlinking poisons pnpm/workspace resolution. |
| Delegate env | Default tight `local()` env; **never** `{ ...process.env }` | review #9: prevents `NAV_DESKTOP_TOKEN`/key leakage into a model shell. |
| Panel safety | `consult_panel` rejects duplicate agents | review #4: two same-agent runs would otherwise race. |
| Merge | `consult`/`consult_panel` return the `worktree` path; Nav diffs it, synthesizes in its own checkout | The returned text is rationale; the real change is in the worktree. |
| Profiles | Reused as each agent's `profile:` baseline | No duplication. |
| Recursion | Only Nav holds the consult tools | Delegates can't re-enter the fleet. |
| Codex gate | Scope `requireCodexProvider` to nav | glm/deepseek don't use Codex. |
| Workflow | **Not** used | review #8: `defineWorkflow` runs one agent harness; doesn't solve cross-agent-with-own-sandbox. Free-chat Nav + consult tools is simpler. |

---

## 7. Verification

1. **Typecheck:** `pnpm --filter @nav/flue typecheck`.
2. **Agents discovered:** boot; `listAgents()` / `GET /api/agents` shows `nav`, `glm`,
   `deepseek-pro`, `deepseek-flash` with `http: true` (route exported — B5).
3. **Direct loopback:** with env set,
   `curl -X POST -H "authorization: Bearer $NAV_DESKTOP_TOKEN" -H 'content-type: application/json' -d '{"message":"reply READY"}' "http://127.0.0.1:$NAV_FLUE_PORT/api/agents/glm/$(uuidgen)?wait=result"`
   returns `{ result: { text: "...READY..." } }` and the call hit `api.z.ai`.
4. **Per-instance isolation:** `consult_panel(["glm","deepseek-pro"], …)` to create the same
   file with different content; confirm two distinct worktrees under `os.tmpdir()/nav-worktrees`,
   each with only its own version, and the main checkout untouched.
5. **Concurrency:** two overlapping `consult("glm", …)` calls land in **different** worktrees
   (distinct instance ids); `consult_panel(["glm","glm"], …)` is rejected.
6. **Snapshot base:** make an uncommitted tracked edit and a non-ignored untracked file in the
   main checkout, delegate, and confirm the delegate sees both (worktree diff is relative to
   the snapshot, not stale HEAD).
7. **No-recursion / no-leak:** confirm glm/deepseek have no `consult` tool and no
   `NAV_DESKTOP_TOKEN` in their shell env.
8. **Compete-and-merge (human test):** drive the real desktop UI; confirm Nav calls
   `consult_panel`, reads both diffs, writes a merged result.
9. **Cleanup:** `pruneAgentWorktrees()` + `git worktree list` is clean after a run.

---

## 8. Risks & open issues (resolve during build)

| Risk / open issue | Notes & suggested handling |
|-------------------|----------------------------|
| **Synthetic snapshot commits are dangling** | Delegation snapshots are real Git objects but not refs. Worktrees keep them reachable while active; after `pruneAgentWorktrees()` removes stale worktrees, normal Git GC can reclaim them. |
| **Toolchain in worktrees** | Worktrees have source but no `node_modules`; **do not** symlink root `node_modules` (pnpm resolves back to the main checkout and breaks isolation — review #6). v1: delegates edit/inspect; Nav runs tests/build after merge. If a delegate must run tests, do a real per-worktree install. |
| **`local()` is not an isolation boundary** | It gives the model the host fs/shell; worktrees separate work **by cwd convention, not enforcement**. Fine for a trusted repo. Enforced per-agent isolation or a Linux toolchain → upgrade each agent's `sandbox` to a **remote sandbox** (Daytona/E2B/Cloudflare): same B topology, stronger boundary. |
| **Secret leakage into delegates** | Never pass `{ ...process.env }` to a delegate `local()` (review #9). Keep API keys at the provider layer only. If a delegate command needs env, pass a tight allowlist excluding `NAV_DESKTOP_TOKEN`/`NAV_FLUE_PORT`/keys. |
| **`?wait=result` longevity** | Per routing-api: synchronous wait is **best-effort, scoped to the admitting process**; if it dies mid-delegation the connection drops and the submission settles in the background (a `submission_settled` event on the agent stream). Acceptable for dev; surface a clean tool error on non-2xx. For robustness later, read the agent stream from `{ streamUrl, offset }`. |
| **Parallel cost (deepseek metered)** | `consult_panel` fires real billed calls for deepseek. Keep deepseek defaults at `high` (not `xhigh`); Nav should panel sparingly. |
| **Port discovery** | Depends on Step 7. If `NAV_FLUE_PORT` is unset (standalone `pnpm dev`), default from the `--port` arg or document exporting it. |
| **Worktree growth** | Per-instance worktrees accumulate; `pruneAgentWorktrees()` at boot + consider reaping a worktree after Nav finishes merging it. |

---

## 9. Migration from current state (glm already ships as a subagent)

Current `main` has glm wired as a **subagent**: `shared/glm.ts` profile + `subagents:[glmProfile]`
on Nav + the task-based delegation instruction + `ensureZaiProvider()` at boot. To move to B:

1. **Remove** from `agents/nav.ts`: the `glmProfile` import, `subagents: [glmProfile]`, and the
   subagent delegation sentence (replaced per Step 5).
2. **Keep** `shared/glm.ts`, `shared/zai-provider.ts` (reused).
3. **Create** `shared/deepseek.ts` + `shared/deepseek-provider.ts` (do not exist yet — source
   in §10), wired as **top-level agents**, not subagents.
4. **Add** Steps 2–4 (worktrees, delegation tools) and the three `agents/*.ts` files.
5. Apply Steps 6–8.

The verified catalog facts and the full provider/profile source needed for all of the above
are in §10; this plan is self-contained.

---

## 10. Appendix — verified catalog facts + provider/profile source (folded in)

Everything below was verified against the installed source and is reused **unchanged** by
Architecture B (the profiles become each top-level agent's `profile:` baseline per Step 1; the
provider bridges are called at boot per Step 6). Rows describing the **subagent** mechanism
(G5, G6, D9) are **historical** — superseded by §1 (B1–B6): the fleet uses top-level agents +
loopback delegation, *not* subagents. They are kept for provenance.

### 10.1 glm — `zai/glm-5.2` catalog facts

| # | Fact | Evidence |
|---|------|----------|
| G1 | `MODELS['zai']['glm-5.2']`: `api:'openai-completions'`, `provider:'zai'`, `baseUrl:'https://api.z.ai/api/coding/paas/v4'` (Coding Plan **global** endpoint), `compat:{thinkingFormat:'zai',supportsReasoningEffort:true,zaiToolStream:true}`, `reasoning:true`, `contextWindow:1_000_000`, `maxTokens:131_072`, `input:['text']`, `cost:{all 0}`. | `pi-ai/dist/models.generated.js` |
| G2 | `thinkingLevelMap` is **binary**: `minimal→null`, `low/medium/high→'high'`, `xhigh→'max'`. Only `xhigh` reaches GLM's "max" effort. | same entry |
| G3 | pi-ai maps provider `zai`→ env `ZAI_API_KEY`. (Separate `zai-coding-cn`→ `open.bigmodel.cn`, its own key — **not used**.) | `pi-ai/dist/env-api-keys.js:81` |
| G4 | `openai-completions` emits GLM reasoning via the `zai` branch: `thinking:{type: effort?'enabled':'disabled'}` + `reasoning_effort = thinkingLevelMap[level]` (so `xhigh`→`'max'`). Auth = `Authorization: Bearer <apiKey>`. | `pi-ai/dist/providers/openai-completions.js:~469` |
| G5 *(historical)* | Flue native subagents: `AgentRuntimeConfig.subagents?: AgentProfile[]` auto-exposes a `task` tool; child = detached session; profiles are **not** HTTP-addressable. **Superseded by §1.** | `guide/subagents`; `action-*.d.mts` |
| G6 *(historical)* | Subagent inheritance: sandbox/cwd fall back to parent; model/thinkingLevel inherit unless declared; instructions/tools profile-owned. **Superseded by §1** (top-level agents own their sandbox). | `guide/subagents` |
| G7 | `registerProvider('zai',{apiKey})` on a **catalog** provider id preserves all catalog metadata and just layers the key. Omitting `apiKey` falls back to pi-ai env lookup. | `api/provider-api` |
| G8 | Resolution: profile `model:'zai/glm-5.2'`+`thinkingLevel` → Flue resolves catalog model → key from boot `registerProvider('zai',…)` → pi-ai `openai-completions` POSTs the Z.ai Coding endpoint. **No per-call wiring.** | Codex-verified path |
| G9 | Desktop spawns the Flue server with `env:{...process.env, NAV_DESKTOP_TOKEN, NAV_DESKTOP_ORIGIN}`, so `ZAI_API_KEY` propagates **iff** it is in the Electron main-process env. | `flue-server.ts:97` |
| G10 | **Unproven offline** (smoke-test only): real Coding-Plan key acceptance + quota. Catalog `cost:0` is no $ signal but Z.ai quota/multipliers are real, so `xhigh`/"max" isn't free. glm is **text-only** → never delegate images. | Z.ai docs |

### 10.2 deepseek — `deepseek/deepseek-v4-pro` + `deepseek-v4-flash` catalog facts

| # | Fact | Evidence |
|---|------|----------|
| D1 | `MODELS['deepseek']['deepseek-v4-pro']`: `api:'openai-completions'`, `provider:'deepseek'`, `baseUrl:'https://api.deepseek.com'`, `reasoning:true`, `contextWindow:1_000_000`, `maxTokens:384_000`, `input:['text']`. **cost** `input:0.435`, `output:0.87`, `cacheRead:0.003625`, `cacheWrite:0` (per M). | `pi-ai/dist/models.generated.js` |
| D2 | `MODELS['deepseek']['deepseek-v4-flash']`: identical shape/provider/baseUrl/ctx/maxTokens/text-only, but **cost** `input:0.14`, `output:0.28`, `cacheRead:0.0028` (per M) — **~3× cheaper** than Pro. | same |
| D3 | Both share `compat:{ thinkingFormat:'deepseek', requiresReasoningContentOnAssistantMessages:true }` and the **same** `thinkingLevelMap:{ minimal:null, low:null, medium:null, high:'high', xhigh:'max' }`. Reasoning is **OFF below `high`**; `high`→"high", `xhigh`→"max". | same |
| D4 | pi-ai maps provider `deepseek` → env **`DEEPSEEK_API_KEY`**. | `pi-ai/dist/env-api-keys.js:73` |
| D5 | The `deepseek` thinkingFormat branch: with a level set it sends `thinking:{type:"enabled"}` + `reasoning_effort = thinkingLevelMap[level]`; none set → `thinking:{type:"disabled"}`. Auth = `Authorization: Bearer <apiKey>`. | `openai-completions.js:495–506` |
| D6 | `supportsReasoningEffort` is *detected* `true` for deepseek (`isDeepSeek` not in the exclusion list). So **`high` vs `xhigh` genuinely reaches the wire** as `reasoning_effort:"high"`/`"max"` — not merely thinking enabled/disabled. | `openai-completions.js:989, 995, 1044` |
| D7 | `requiresReasoningContentOnAssistantMessages:true` is **handled automatically** by the provider (injects empty `reasoning_content:""` before re-sending history). **No action in our code.** | `openai-completions.js:809–812, 1001` |
| D8 | A separate `MODELS['opencode']['deepseek-v4-flash-free']` exists (provider `opencode`, `opencode.ai/zen/v1`, **200k** ctx, no `deepseek` thinkingFormat, `cost:0`). **Not** the `deepseek/*` model; out of scope. | `models.generated.js` |
| D9 *(historical)* | The subagent/inheritance/registration facts (G5–G9) applied unchanged to deepseek under the old subagent design. **Superseded by §1.** | n/a |
| D10 | **Unproven offline** (smoke-test only): key acceptance at `api.deepseek.com` + live billing. Both **text-only** → never delegate images. Both **metered** (real $), unlike glm's flat plan → default `high`, not `xhigh`. | DeepSeek docs |

### 10.3 Provider bridges

`ensureZaiProvider` (already shipped) and `ensureDeepseekProvider` (new) are called once at
boot (Step 6). One `registerProvider("deepseek", …)` serves **both** deepseek models.

```ts
// shared/zai-provider.ts  (EXISTS on main)
import { registerProvider } from "@flue/runtime";
const PROVIDER_ID = "zai";
let registered = false;
export function ensureZaiProvider(): void {
  if (registered) return;
  const apiKey = process.env.ZAI_API_KEY?.trim();
  if (!apiKey) {
    console.warn("[nav] ZAI_API_KEY is not set; the glm agent is unavailable until it is configured.");
    return;
  }
  registerProvider(PROVIDER_ID, { apiKey });
  registered = true;
}
```

```ts
// shared/deepseek-provider.ts  (NEW)
import { registerProvider } from "@flue/runtime";
const PROVIDER_ID = "deepseek";
let registered = false;
// `deepseek` is a pi-ai catalog provider (https://api.deepseek.com, openai-completions,
// thinkingFormat "deepseek", 1M ctx). One registration hydrates BOTH v4-pro and v4-flash —
// baseUrl/reasoning/thinkingLevelMap carry through; do NOT re-specify them. Metered $/token,
// static Bearer key (no OAuth/refresh). No-op + warning when the key is unset.
export function ensureDeepseekProvider(): void {
  if (registered) return;
  const apiKey = process.env.DEEPSEEK_API_KEY?.trim();
  if (!apiKey) {
    console.warn("[nav] DEEPSEEK_API_KEY is not set; the deepseek-pro and deepseek-flash agents are unavailable until it is configured.");
    return;
  }
  registerProvider(PROVIDER_ID, { apiKey });
  registered = true;
}
```

### 10.4 Profiles (reused as each top-level agent's `profile:` baseline)

The "prefer read-only … unless the delegating agent explicitly asks you to make changes" line
is intentional: in Architecture B, Nav's `consult` prompt explicitly authorizes edits in the
delegate's own worktree, so delegates *do* edit when asked, and stay read-only otherwise.
glm defaults to `xhigh` (flat-rate); both deepseek tiers default to `high` (metered).

```ts
// shared/glm.ts  (EXISTS on main)
import { defineAgentProfile, type ThinkingLevel } from "@flue/runtime";
const validThinkingLevels = ["minimal","low","medium","high","xhigh"] as const;
type GlmThinkingLevel = (typeof validThinkingLevels)[number];
function isGlmThinkingLevel(v: string): v is GlmThinkingLevel { return validThinkingLevels.includes(v as GlmThinkingLevel); }
// Default xhigh → GLM "max". Map is binary (low/med/high→"high", only xhigh→"max"); glm exists
// to bring MORE reasoning than gpt-5.5. Dial DOWN with NAV_GLM_THINKING_LEVEL=high.
function resolveGlmThinkingLevel(): ThinkingLevel {
  const c = process.env.NAV_GLM_THINKING_LEVEL?.trim();
  return c && isGlmThinkingLevel(c) ? c : "xhigh";
}
export const glmProfile = defineAgentProfile({
  name: "glm",
  description:
    "Senior full-stack engineer (L3, GLM-5.2, 1M context). Delegate the hard, ambiguous, high-judgment work anywhere in the stack: architecture and design tradeoffs, deep root-cause analysis, plan/code review, broad large-context exploration. Trust and build on its conclusions. Not for trivial lookups or image inputs (text-only).",
  model: "zai/glm-5.2",
  thinkingLevel: resolveGlmThinkingLevel(),
  instructions: [
    "You are glm, a senior (L3) full-stack engineer the Nav lead delegates hard problems to in the Nav monorepo.",
    "Investigate independently with your file and command tools, cite code as path:line, challenge assumptions, and bring senior-level rigor: state your assumptions, weigh alternatives, and give a clear recommendation, not just a list of options.",
    "Prefer read-only analysis. Do not create, modify, or delete files, and do not run mutating commands, unless the delegating agent explicitly asks you to make changes.",
  ].join(" "),
});
```

```ts
// shared/deepseek.ts  (NEW)
import { defineAgentProfile, type ThinkingLevel } from "@flue/runtime";
const validThinkingLevels = ["minimal","low","medium","high","xhigh"] as const;
type DeepseekThinkingLevel = (typeof validThinkingLevels)[number];
function isDeepseekThinkingLevel(v: string): v is DeepseekThinkingLevel { return validThinkingLevels.includes(v as DeepseekThinkingLevel); }
// Default high for both. deepseek's map turns reasoning OFF for minimal/low/medium; `high` is
// the minimum that enables it, `xhigh`→"max". DeepSeek is METERED, so do NOT default to xhigh.
// Per-tier env: NAV_DEEPSEEK_PRO_THINKING_LEVEL / NAV_DEEPSEEK_FLASH_THINKING_LEVEL.
function resolveThinkingLevel(envVar: string): ThinkingLevel {
  const c = process.env[envVar]?.trim();
  return c && isDeepseekThinkingLevel(c) ? c : "high";
}
export const deepseekProProfile = defineAgentProfile({
  name: "deepseek-pro",
  description:
    "Junior full-stack engineer (L2, DeepSeek V4 Pro, 1M context). Delegate well-scoped, clearly-specified, mostly-mechanical work: targeted implementation against a precise spec, focused refactors, writing tests, mechanical migrations, structured extraction. Give it exact instructions and review its output. Not for ambiguous or high-judgment design work, and not for image inputs (text-only).",
  model: "deepseek/deepseek-v4-pro",
  thinkingLevel: resolveThinkingLevel("NAV_DEEPSEEK_PRO_THINKING_LEVEL"),
  instructions: [
    "You are deepseek-pro, a junior (L2) full-stack engineer the Nav lead delegates well-scoped, clearly-specified work to in the Nav monorepo.",
    "Follow the spec precisely and stay in scope. Use your file and command tools to do exactly what was asked, and cite code as path:line.",
    "If the task is ambiguous or under-specified, do NOT guess at intent — state what is unclear in your result and ask the lead to clarify.",
    "Prefer read-only analysis. Do not create, modify, or delete files, and do not run mutating commands, unless the delegating agent explicitly asks you to make changes.",
  ].join(" "),
});
export const deepseekFlashProfile = defineAgentProfile({
  name: "deepseek-flash",
  description:
    "Fresh-grad full-stack engineer (L1, DeepSeek V4 Flash, 1M context — cheapest & fastest). Delegate small, trivial, fully-specified mechanical tasks: boilerplate, simple edits across known locations, rename/format passes, quick structured lookups. Spell out exactly what to do and verify the result. Not for anything needing judgment or ambiguity, and not for image inputs (text-only).",
  model: "deepseek/deepseek-v4-flash",
  thinkingLevel: resolveThinkingLevel("NAV_DEEPSEEK_FLASH_THINKING_LEVEL"),
  instructions: [
    "You are deepseek-flash, a fresh-grad (L1) full-stack engineer the Nav lead delegates small, fully-specified mechanical tasks to in the Nav monorepo.",
    "Do exactly and only what the task specifies — do not expand scope or make design decisions. Use your file and command tools, and cite code as path:line.",
    "If anything is unclear, stop and say so in your result rather than guessing.",
    "Prefer read-only analysis. Do not create, modify, or delete files, and do not run mutating commands, unless the delegating agent explicitly asks you to make changes.",
  ].join(" "),
});
```
