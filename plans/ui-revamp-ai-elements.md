# nav Electron UI revamp → AI Elements (sequential execution plan)

> Agreed by **Claude (Opus 4.8)** and **Codex (gpt, codex-cli 0.142.1)** after two rounds of
> review. Codex verified every codebase claim against the repo and signed off on this v2.
> Written for a **weak coding agent** that executes **one step at a time** and can run
> long tasks. Do exactly what each step says. Do not improvise or batch steps.

---

## 0. What this is

Replace the renderer's hand-built UI with **AI Elements** (Vercel's shadcn-based AI component
library, https://elements.ai-sdk.dev) for the **chat surface**, and **shadcn/ui** primitives for
everything else (shell, sidebar, tables). The app stays **Vite + React 19 + Electron** — **no
Next.js**. This is a **visual/UX revamp**: keeping the exact old behavior matters less than
landing the new components and looking good, BUT you must not break launch / send / stop.

Renderer lives in `desktop/electron/renderer/`. Source is `desktop/electron/renderer/src/`.
Deps and `tsconfig.json` live at the **repo root** (it is a pnpm workspace). The renderer has
**no** `package.json` of its own.

---

## 1. Ground rules — RE-READ BEFORE EVERY STEP

0. **Run unattended. Never ask a human a question.** There is no human watching. Every decision
   is already made in this plan. Do not pause for confirmation, clarification, or approval — just
   execute the steps in order. ("Halt" below means *stop working and write a report file*, NOT
   *ask a question and wait* — never wait on human input.)
1. **One step at a time.** Finish a step, run its DONE CHECKLIST (§3), commit, then move on.
2. **Never run interactive prompts.** Every CLI command here already has its non-interactive flag
   (`--yes`, `-c .`). Never type an answer into a prompt. If a command unexpectedly blocks on
   input, kill it, try the documented fallback once, and if it still blocks, **halt** (§ below).
3. **Never `git add -A` or `git add .`** Stage only the files the step changed, by name.
4. **Format before lint** (formatting can dirty files lint already checked).
5. **If a step still fails after two repair attempts, HALT** — do not thrash or invent
   workarounds. To halt: append the failing step number + the exact error + last command output
   to `plans/revamp-progress.md`, leave the working tree as-is (uncommitted), and stop. A human
   will read the report later. Do not ask a question, do not skip ahead to other phases.
6. **Linter/formatter is Biome**, not eslint/prettier: `pnpm run lint`, `pnpm run format`. Run
   these binaries directly; do not let a proxy rewrite them.
7. **Package manager is pnpm** (workspace). Install at root with `-w`.
8. **Do not delete old CSS, deps, or files until an `rg` search proves nothing imports them.**
9. **Visual check = `agent-browser`** against the running app (see §3 step 5). Never Playwright.
10. Commit messages use the prefix `revamp: `.

---

## 2. Component mapping (reference)

| Old (custom) | New |
|---|---|
| `Transcript.tsx` scroll list | AI Elements `conversation` (`Conversation`, `ConversationContent`, `ConversationScrollButton`) |
| `MessageRow` (user/assistant bubbles) | AI Elements `message` (`Message`, `MessageContent`) |
| `MarkdownText` (marked+dompurify) | the markdown renderer exported by AI Elements `message` (e.g. `Response`/`MessageResponse` — **read the generated file for the exact name**); uses Streamdown+Shiki |
| `ToolMessageRow` | AI Elements `tool` (`Tool`, `ToolHeader`, `ToolContent`, `ToolInput`, `ToolOutput`) |
| `Composer` textarea + send/stop | AI Elements `prompt-input` (`PromptInput`, `PromptInputTextarea`, `PromptInputToolbar`, `PromptInputSubmit`) |
| `ModelMenu` | AI Elements `model-selector` |
| token usage line | AI Elements `context` |
| `ThinkingMenu`, `SessionModeMenu` | shadcn `select` |
| running indicator | AI Elements `loader` |
| empty / welcome state | AI Elements `suggestion` |
| `SessionToolbar` tabs | shadcn `tabs` + `button` |
| `Sidebar` | shadcn `sidebar` (keep all grouping/collapse/attention/running logic) |
| `SettingsPage` model table | shadcn `table` + `badge` + `select` (keep TanStack Table state) |
| `StacksPage` trace table | shadcn `table` (optionally evaluate AI Elements `stack-trace`) |

> **There is NO standalone `response` component** in the AI Elements registry — the markdown
> renderer ships inside `message`. Do not try to `add response`.

---

## 3. DONE CHECKLIST (the verification gate for EVERY step)

Run these in this exact order. All must pass before you commit.

```bash
pnpm run format                       # 1. format (Biome)
pnpm run lint                         # 2. lint (Biome). If it fails: `pnpm run lint:fix` then re-run.
pnpm run check:electron               # 3. typecheck + all builds + node tests. Must exit 0.
pnpm run electron:smoke               # 4. build + launch Electron headless (--smoke). Must exit 0.
```
5. **Visual artifact (no human gate):** start the app (`pnpm run electron:dev`), then in another
   shell drive it with `agent-browser` to capture a screenshot of the surface you changed (or use
   the smoke screenshot). Save it under `plans/revamp-shots/<step>.png` and append a one-line note
   to `plans/revamp-progress.md`. The screenshots are for a human to review **asynchronously** —
   do **not** wait for confirmation. The pass/fail decision for the step rests on the automated
   gates (steps 3–4 above) plus a sanity check that the screenshot is not blank/all-error. If the
   screenshot is blank or shows an error overlay, treat the step as failed and apply rule 5.
6. `git status --short` and `git diff` — review what changed.
7. `git add <only the files this step changed, by name>` (NOT `-A`, NOT `-p`).
8. `git commit -m "revamp: <what this step did>"`.

If any of 1–5 fails: fix it (max two tries), else STOP and report.

---

## PHASE 0 — Foundation + smoke proof

### Step 0.0 — Record a green baseline
```bash
pnpm install
pnpm run check:electron
pnpm run electron:smoke
```
Both must already pass on a clean checkout. If not, **halt** (rule 5) — do not start the revamp on
a red baseline. (No commit; nothing changed.)

### Step 0.1 — Add Tailwind + libs and wire the `@` alias
Install (build tooling as dev deps, the rest as deps):
```bash
pnpm add -w -D tailwindcss @tailwindcss/vite
pnpm add -w class-variance-authority clsx tailwind-merge tw-animate-css ai @ai-sdk/react lucide-react
```
Edit `desktop/electron/renderer/vite.config.ts` to add the Tailwind plugin and the `@` alias.
Final file:
```ts
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const rendererRoot = fileURLToPath(new URL(".", import.meta.url));

export default defineConfig({
  root: rendererRoot,
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  build: {
    emptyOutDir: true,
    outDir: "dist",
    rolldownOptions: { output: { codeSplitting: true } },
    sourcemap: true,
  },
});
```
Edit `tsconfig.json` (repo root) — add `baseUrl` and `paths` inside `compilerOptions`:
```jsonc
"baseUrl": ".",
"paths": { "@/*": ["desktop/electron/renderer/src/*"] },
```
**Verify:** DONE CHECKLIST. (App still looks identical — no Tailwind imported into CSS yet.)
Commit: `revamp: add tailwind v4, deps, and @ path alias`.

### Step 0.2 — Theme CSS, `cn()` util, components.json, first shadcn Button + preflight gate
Create `desktop/electron/renderer/src/lib/utils.ts`:
```ts
import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
```
Create `desktop/electron/renderer/styles/theme.css` (ported from `styles/base.css` oklch palette):
```css
@import "tailwindcss";
@import "tw-animate-css";

/* Tailwind v4 must scan Streamdown's prebuilt classes or markdown/code won't be
   styled. Path is relative to THIS file → repo-root node_modules (up 4 dirs).
   After adding the `message` component (Step 0.3), confirm this path resolves. */
@source "../../../../node_modules/streamdown/dist/*.js";

@custom-variant dark (&:is(.dark *));

:root {
  --radius: 0.625rem;
  --background: oklch(0.21 0.004 95);
  --foreground: oklch(0.98 0.004 95);
  --card: oklch(0.29 0.016 264);
  --card-foreground: oklch(0.98 0.004 95);
  --popover: oklch(0.29 0.016 264);
  --popover-foreground: oklch(0.98 0.004 95);
  --primary: oklch(0.72 0.08 176);
  --primary-foreground: oklch(0.21 0.004 95);
  --secondary: oklch(0.39 0.012 261);
  --secondary-foreground: oklch(0.98 0.004 95);
  --muted: oklch(0.29 0.016 264);
  --muted-foreground: oklch(0.72 0.004 95);
  --accent: oklch(0.39 0.012 261);
  --accent-foreground: oklch(0.98 0.004 95);
  --destructive: oklch(0.66 0.13 25);
  --border: oklch(0.34 0.01 264);
  --input: oklch(0.34 0.01 264);
  --ring: oklch(0.72 0.08 176);
  --sidebar: oklch(0.35 0.024 266);
  --sidebar-foreground: oklch(0.98 0.004 95);
  --sidebar-primary: oklch(0.72 0.08 176);
  --sidebar-primary-foreground: oklch(0.21 0.004 95);
  --sidebar-accent: oklch(0.39 0.012 261);
  --sidebar-accent-foreground: oklch(0.98 0.004 95);
  --sidebar-border: oklch(0.34 0.01 264);
  --sidebar-ring: oklch(0.72 0.08 176);
}

@theme inline {
  --color-background: var(--background);
  --color-foreground: var(--foreground);
  --color-card: var(--card);
  --color-card-foreground: var(--card-foreground);
  --color-popover: var(--popover);
  --color-popover-foreground: var(--popover-foreground);
  --color-primary: var(--primary);
  --color-primary-foreground: var(--primary-foreground);
  --color-secondary: var(--secondary);
  --color-secondary-foreground: var(--secondary-foreground);
  --color-muted: var(--muted);
  --color-muted-foreground: var(--muted-foreground);
  --color-accent: var(--accent);
  --color-accent-foreground: var(--accent-foreground);
  --color-destructive: var(--destructive);
  --color-border: var(--border);
  --color-input: var(--input);
  --color-ring: var(--ring);
  --radius-sm: calc(var(--radius) - 4px);
  --radius-md: calc(var(--radius) - 2px);
  --radius-lg: var(--radius);
  --radius-xl: calc(var(--radius) + 4px);
  --color-sidebar: var(--sidebar);
  --color-sidebar-foreground: var(--sidebar-foreground);
  --color-sidebar-primary: var(--sidebar-primary);
  --color-sidebar-primary-foreground: var(--sidebar-primary-foreground);
  --color-sidebar-accent: var(--sidebar-accent);
  --color-sidebar-accent-foreground: var(--sidebar-accent-foreground);
  --color-sidebar-border: var(--sidebar-border);
  --color-sidebar-ring: var(--sidebar-ring);
}
```
Edit `desktop/electron/renderer/styles.css` — add the theme import as the **FIRST** line (before
`base.css`):
```css
@import "./styles/theme.css"; /* tailwind layers + shadcn tokens — must be first */
@import "./styles/base.css";
/* ...rest unchanged... */
```
> If `tests/electron_renderer_styles.test.cts` asserts the exact list/order of `styles.css`
> imports, **update that test** to include `theme.css` first, or `check:electron` will fail.

Add the dark class so shadcn dark tokens apply. In `desktop/electron/renderer/index.html`, set the
root html tag to `<html lang="en" class="dark">` (find the `<html...>` tag and add `class="dark"`).

Create `components.json` at the **repo root**:
```json
{
  "$schema": "https://ui.shadcn.com/schema.json",
  "style": "new-york",
  "rsc": false,
  "tsx": true,
  "tailwind": {
    "config": "",
    "css": "desktop/electron/renderer/styles.css",
    "baseColor": "neutral",
    "cssVariables": true,
    "prefix": ""
  },
  "aliases": {
    "components": "@/components",
    "ui": "@/components/ui",
    "utils": "@/lib/utils",
    "lib": "@/lib",
    "hooks": "@/hooks"
  },
  "iconLibrary": "lucide"
}
```
Add the first shadcn primitive (non-interactive, explicit config dir `-c .`):
```bash
pnpm dlx shadcn@latest add button -c . --yes
```
> This must write `desktop/electron/renderer/src/components/ui/button.tsx`. If it writes anywhere
> else, or prompts, **halt** (rule 5) — the alias/components.json is wrong.

Create a temporary smoke component `desktop/electron/renderer/src/components/_RevampSmoke.tsx`:
```tsx
import { Button } from "@/components/ui/button";

// AI-ELEMENTS-SMOKE-OK
export function RevampSmoke() {
  return (
    <div className="flex flex-col gap-3 rounded-lg border bg-card p-4 text-card-foreground">
      <p className="text-sm text-muted-foreground">revamp smoke</p>
      <Button>Tailwind + shadcn OK</Button>
    </div>
  );
}
```
Render `<RevampSmoke />` somewhere visible in the **empty/welcome chat view** in `App.tsx` (the
no-session state). Keep it until Step 1.2 replaces the transcript, then delete it.

Create the static foundation test `tests/revamp_setup.test.cts` — model it on the existing
`tests/electron_renderer_styles.test.cts` (same `node:test` + `fs` style). Assert:
- `styles.css`'s first `@import` is `./styles/theme.css`.
- `theme.css` contains `@import "tailwindcss"`, `--primary`, and `streamdown`.
- root `components.json` exists.
- `src/components/_RevampSmoke.tsx` contains `AI-ELEMENTS-SMOKE-OK`.

**Verify:** DONE CHECKLIST. This is the **"did Tailwind preflight wreck the look"** gate — look
hard at the app. The smoke card + button must render with the dark theme. Existing surfaces should
still look right (custom CSS is unlayered so it keeps winning over Tailwind base; if something
broke, an unlayered `base.css` rule is the suspect). Fix before continuing.
Commit: `revamp: tailwind theme, cn util, components.json, shadcn button smoke`.

### Step 0.3 — Prove AI Elements markdown renders in Electron
Add the message component:
```bash
pnpm dlx shadcn@latest add @ai-elements/message -c . --yes
```
> Falls back to `pnpm dlx ai-elements@latest add message` if the above prompts/fails.
Open the generated `desktop/electron/renderer/src/components/ai-elements/message.tsx`. **Note the
exact exported name of the markdown renderer** (e.g. `Response` or `MessageResponse`) and the
deps it imports.
```bash
# Confirm Streamdown/Shiki got installed; if missing, add them:
rg -n "streamdown|shiki|use-stick-to-bottom" package.json || pnpm add -w streamdown shiki use-stick-to-bottom
```
In `_RevampSmoke.tsx`, also render the markdown component with a hardcoded string containing a
heading, a list, and a fenced code block (```` ```ts ... ``` ````). This proves Streamdown + Shiki
work under Electron's sandbox.
**Verify:** DONE CHECKLIST. The markdown must render formatted, and the code block must be
syntax-highlighted in the Electron window. If code is unstyled, the `@source` path in `theme.css`
is wrong — fix it (it must point at the real `node_modules/streamdown/dist`).
Commit: `revamp: prove AI Elements message/markdown renders in Electron`.

---

## PHASE 1 — Chat transcript

### Step 1.1 — Pure adapter (data → AI Elements props), with a unit test
Create `desktop/electron/renderer/src/lib/ai-elements-adapter.ts`: a **pure** function mapping the
existing store types (`ChatMessage`, `ToolMessage` from `src/types.ts`) into the props the AI
Elements components need. Do **not** use `useChat`. Map roles `user`/`assistant`/`error`. Leave a
`TODO` for tool-state mapping (filled in 1.4 after reading the generated `tool.tsx`).
Add a unit test `tests/revamp_adapter.test.cts` (model on existing `node:test` tests) covering the
mapping for each role.
**Verify:** DONE CHECKLIST. Commit: `revamp: add pure session→AI-Elements adapter + test`.

### Step 1.2 — Rebuild the transcript on `conversation` + `message`
```bash
pnpm dlx shadcn@latest add @ai-elements/conversation -c . --yes
```
Rewrite `Transcript.tsx` to render `Conversation` → `ConversationContent` → one `Message` +
`MessageContent` per message, fed by the Step 1.1 adapter. Keep `marked` for assistant text **for
now** (wrap the existing markdown HTML, or render plain text). Delete the temporary
`<RevampSmoke />` render and `_RevampSmoke.tsx`. It is OK to drop `@tanstack/react-virtual` from
**Transcript** (the AI Elements `Conversation` handles autoscroll) — but do **not** uninstall the
package yet; `Sidebar` still uses it.
**Verify:** DONE CHECKLIST. Messages render, new messages autoscroll to bottom.
Commit: `revamp: rebuild transcript on AI Elements conversation+message`.

### Step 1.3 — Swap markdown to Streamdown, retire `marked` in the transcript
Replace `MarkdownText` with the markdown renderer from `message.tsx` (the name you noted in 0.3).
Remove `marked`/`dompurify` usage **from the transcript only**.
```bash
rg -n "marked|dompurify|renderMarkdown|MarkdownText" desktop/electron/renderer/src
```
Anything still referencing them stays untouched (cleaned up in Phase 5). Remove the now-dead
markdown rules from `styles/transcript.css`.
**Verify:** DONE CHECKLIST. Commit: `revamp: render assistant markdown via Streamdown`.

### Step 1.4 — Tool calls → AI Elements `tool`
```bash
pnpm dlx shadcn@latest add @ai-elements/tool -c . --yes
```
Open the generated `tool.tsx`, read its TypeScript prop/state types, **then** finish the
tool-state mapping in the adapter (1.1's TODO): map `ToolMessage.state` (`running`/`completed`/
`failed`) onto the states `tool.tsx` actually accepts. Render `ToolMessage`s as `Tool` in the
transcript. Remove dead tool CSS from `styles/transcript.css`.
**Verify:** DONE CHECKLIST. Running/done/failed tool rows look right.
Commit: `revamp: render tool calls via AI Elements tool`.

### Step 1.5 — Reasoning (only if the backend emits thinking text)
```bash
rg -n "reasoning|thinking|thought" desktop/electron/renderer/src/types.ts desktop/electron/renderer/src/lib
```
If the session stream carries reasoning/thinking text, `add @ai-elements/reasoning` and render it.
If not, **skip this step** and write a one-line note in the commit-less progress log. (No commit
if skipped.)

---

## PHASE 2 — Composer

### Step 2.1 — `prompt-input` replaces textarea + send/stop
```bash
pnpm dlx shadcn@latest add @ai-elements/prompt-input -c . --yes
```
Rebuild the input area of `Composer.tsx` with `PromptInput`/`PromptInputTextarea`/
`PromptInputToolbar`/`PromptInputSubmit`. **Preserve these behaviors exactly** (see
`Composer.tsx:72` area): the `onSend` contract, Enter-to-send, the localStorage draft persistence,
disabled state while running, and the Stop button when a run is active.
**Verify:** DONE CHECKLIST. You can type, Enter sends, Stop appears during a run and cancels.
Commit: `revamp: rebuild composer input on AI Elements prompt-input`.

### Step 2.2 — `model-selector` + `context`
```bash
pnpm dlx shadcn@latest add @ai-elements/model-selector @ai-elements/context -c . --yes
```
Replace `ModelMenu` with `model-selector` (keep the same model list + selection callback) and the
token-usage line with `context`.
**Verify:** DONE CHECKLIST. Model switching still updates the session; token counts show.
Commit: `revamp: model-selector + context for composer metadata`.

### Step 2.3 — Thinking + session-mode menus → shadcn `select`
```bash
pnpm dlx shadcn@latest add select -c . --yes
```
Replace `ThinkingMenu` and `SessionModeMenu` with shadcn `Select`, preserving their option lists
and callbacks. Remove the now-dead rules from `styles/composer.css`.
**Verify:** DONE CHECKLIST. Commit: `revamp: thinking + session-mode menus on shadcn select`.

---

## PHASE 3 — Shell + sidebar

### Step 3.1 — Session toolbar tabs → shadcn `tabs`
```bash
pnpm dlx shadcn@latest add tabs -c . --yes
```
Rebuild `SessionToolbar` (in `App.tsx`) chat/stacks/settings tabs with shadcn `Tabs`, keeping the
TanStack Router navigation wired to tab changes.
**Verify:** DONE CHECKLIST. Tabs switch views and reflect the active route.
Commit: `revamp: session toolbar tabs on shadcn tabs`.

### Step 3.2 — Sidebar → shadcn `sidebar` (wrap first, migrate internals second)
```bash
pnpm dlx shadcn@latest add sidebar -c . --yes
```
**Do not one-shot rewrite.** First wrap the existing `Sidebar` content in the shadcn sidebar shell
(`SidebarProvider`/`Sidebar`/`SidebarContent`), verify it launches, commit. **Then** migrate the
internals (projects/sessions groups) to `SidebarGroup`/`SidebarMenu`, preserving the existing
grouping, collapse/expand, attention, and running indicators (logic around `Sidebar.tsx:37`). It
is fine to drop `@tanstack/react-virtual` from the sidebar here.
**Verify:** DONE CHECKLIST after each of the two commits. Sidebar groups, collapse, +New, and
running/attention dots all work.
Commits: `revamp: wrap sidebar in shadcn shell` then `revamp: migrate sidebar internals to shadcn`.

### Step 3.3 — Remove `@tanstack/react-virtual`
```bash
rg -n "react-virtual|useVirtualizer" desktop/electron/renderer/src
```
Only if there are **zero** matches: `pnpm remove -w @tanstack/react-virtual`.
**Verify:** DONE CHECKLIST. Commit: `revamp: drop @tanstack/react-virtual`.

---

## PHASE 4 — Secondary pages

### Step 4.1 — Settings model table → shadcn
```bash
pnpm dlx shadcn@latest add table badge -c . --yes
```
Restyle `SettingsPage.tsx` with shadcn `Table` + `Badge` (+ the `Select` from 2.3). **Keep the
TanStack Table state/logic**; only swap the DOM/styling. Remove dead `styles/settings.css` rules.
**Verify:** DONE CHECKLIST. Commit: `revamp: settings model table on shadcn table`.

### Step 4.2 — Stacks trace table → shadcn
Restyle `StacksPage.tsx` with shadcn `Table`. Optionally evaluate AI Elements `stack-trace`
(`add @ai-elements/stack-trace`) — if it doesn't fit the API-trace data, fall back to shadcn
`Table`. Keep the TanStack Table state. Remove dead `styles/stacks.css` rules.
**Verify:** DONE CHECKLIST. Commit: `revamp: stacks trace table on shadcn table`.

---

## PHASE 5 — Polish + cleanup

### Step 5.1 — Empty state + running indicator
```bash
pnpm dlx shadcn@latest add @ai-elements/suggestion @ai-elements/loader -c . --yes
```
Use `suggestion` for the empty/welcome chat state (a few suggested prompts that call `onSend`) and
`loader` for the run-in-progress indicator.
**Verify:** DONE CHECKLIST. Commit: `revamp: suggestion empty-state + loader`.

### Step 5.2 — Remove dead CSS and deps
```bash
rg -n "marked|dompurify" desktop/electron/renderer/src        # must be ZERO matches
```
Only if zero matches: `pnpm remove -w marked dompurify` and delete `src/lib/markdown.ts` if unused.
Delete any fully-replaced CSS partials and their `@import` lines in `styles.css` (verify each is
unreferenced first). Keep `theme.css` and any partial still in use.
**Final verify:** run the full DONE CHECKLIST one more time, plus a careful visual pass of every
view (chat, stacks, settings, sidebar, empty state). 
Commit: `revamp: remove dead markdown deps and replaced CSS`.

---

## 4. Risks & fallbacks

- **shadcn `add` writes to the wrong path or prompts** → the `@` alias / `components.json` is
  misconfigured. Re-check Step 0.1/0.2. Always pass `-c . --yes`.
- **Tailwind preflight changes the look** → expected, caught at Step 0.2. Existing custom CSS is
  unlayered and wins over Tailwind's layered base, so damage is limited; if a shadcn component
  looks unstyled, hunt for a broad unlayered rule in `base.css` overriding it and scope that rule.
- **Code blocks not highlighted** → the Streamdown `@source` path in `theme.css` is wrong. It must
  resolve from `theme.css` to repo-root `node_modules/streamdown/dist`.
- **AI Elements component export names differ from this doc** → always open the generated file
  under `src/components/ai-elements/` and use its actual exports. The registry/API moves; the
  generated source is the source of truth.
- **Electron CSP** → there is no CSP today (`window-options.cts`), so inline Shiki styles are
  allowed. If someone later adds a CSP `<meta>`, it must include `style-src 'unsafe-inline'` (or
  switch Shiki to its CSS-variables theme).
- **A migration breaks send/stop/model/launch** → these contracts live in `App.tsx` (run/stop
  bookkeeping, model refresh, route state). Revert that step's commit and redo, preserving the
  prop contracts.

## 5. Sign-off
This plan was debated to agreement by Claude and Codex. Codex's final verdict: **"yes, I agree"**
with the v2 sequencing, the hand-authored `components.json`/`theme.css` foundation, AI Elements for
chat + shadcn for shell, thin adapters (no `useChat`), early smoke proof, late markdown/dependency
removal, and static-assertion tests over a DOM harness.
