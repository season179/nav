Before finishing, run the linter and formatter and make sure everything is okay.
Final response must be easy to understand and concise.
Prefer to use UUID v7 over v4 when it makes sense.
Do not manually modify generated shadcn UI components under `packages/desktop/src/components/ui` and `packages/desktop/src/components/ai-elements` unless the user explicitly asks for component-level changes.
Do not over-engineer!
Do not over-think!

## Flue agent runtime (`packages/flue`)

Nav's backend is a **Flue** app (`@flue/runtime@1.0.0-beta.7`, models via `@earendil-works/pi-ai@0.79.10`). Source is under `packages/flue/.flue/`:
- `agents/*.ts` — each file with `export default defineAgent(...)` is auto-discovered and served at `/api/agents/<name>/:id` (add `export const route` to expose it). `shared/*.ts` is **not** agent-discovered — put helpers and reusable `defineAgentProfile(...)` profiles there.
- `app.ts` — Hono entry (CORS, desktop auth, provider warm-up at boot). `db.ts` — durable sqlite session store. `flue.config.ts` — `target: "node"`.
- Authoritative docs: `cd packages/flue && pnpm exec flue docs read <path>` (e.g. `guide/subagents`, `guide/models`, `api/provider-api`, `guide/sandboxes`). Verify runtime types in `node_modules/.pnpm/@flue+runtime@*/node_modules/@flue/runtime/dist/*.d.mts`.

**Models & providers.** A model spec is `provider/model` (e.g. `openai-codex/gpt-5.5`, `zai/glm-5.2`, `deepseek/deepseek-v4-pro`). Valid specs and all per-model metadata (`api` wire protocol, `baseUrl`, `reasoning`, `thinkingLevelMap`, `contextWindow`, `maxTokens`, `compat`) come from pi-ai's catalog — **read it, don't guess**: `node_modules/.pnpm/@earendil-works+pi-ai@*/node_modules/@earendil-works/pi-ai/dist/models.generated.js`. `registerProvider(id, { apiKey })` (from `@flue/runtime`) on a **catalog** id just layers the key onto the catalog entry (baseUrl/api/reasoning all carry through — don't re-specify them); omitting `apiKey` falls back to pi-ai's per-provider env var (see `dist/env-api-keys.js`, e.g. `zai`→`ZAI_API_KEY`, `deepseek`→`DEEPSEEK_API_KEY`). Non-catalog ids must supply `api`+`baseUrl`. Live providers: `openai-codex/gpt-5.5` (ChatGPT/Codex subscription, OAuth bearer from `~/.codex/auth.json`, refreshed — `shared/codex-provider.ts`), `zai/glm-5.2` (Z.ai GLM Coding Plan, static `ZAI_API_KEY`, Bearer auth, global endpoint `https://api.z.ai/api/coding/paas/v4` — `shared/zai-provider.ts`), and `deepseek/deepseek-v4-pro` / `deepseek/deepseek-v4-flash` (static `DEEPSEEK_API_KEY`, Bearer auth, global endpoint `https://api.deepseek.com` — `shared/deepseek-provider.ts`).

**Thinking levels.** `thinkingLevel` (`minimal|low|medium|high|xhigh`, harness default `medium`) is mapped per-model by its `thinkingLevelMap`. It is **not** linear — for `zai/glm-5.2` it's binary: `low/medium/high`→GLM `"high"`, only `xhigh`→GLM `"max"`; for DeepSeek v4, `minimal/low/medium` disable reasoning, `high`→`"high"`, and `xhigh`→`"max"`. Check the map before assuming a level does anything.

**Delegation/fleet.** Nav uses top-level delegate agents, not Flue subagents. `agents/glm.ts`, `agents/deepseek-pro.ts`, and `agents/deepseek-flash.ts` export `route` and run through loopback HTTP from Nav's `consult` / `consult_panel` tools (`shared/delegation.ts`). Each delegate gets a per-delegation git worktree outside the checkout (`shared/worktrees.ts`), while Nav keeps the final edit in the real checkout. Delegate profiles live in `shared/*.ts`; do not give delegates the consult tools or copy `process.env` into their `local({ cwd })` sandbox.

## Codex CLI (consult/review)

`codex exec resume <SESSION_ID> "<prompt>"` has a **different flag grammar** than `codex exec`. It rejects `-C/--cwd` and `-s/--sandbox` (resume inherits cwd + sandbox from the original session); valid flags are `-c`, `--enable`, `--last`, `--all`, `--json`. When a resume fails with `unexpected argument '-C'` / `For more information, try '--help'`, strip **all** `codex exec`-only flags at once — don't peel them off one at a time.
