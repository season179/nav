# Revamp progress

## Step 0.1 — Add Tailwind + libs and wire the `@` alias
- pnpm add: tailwindcss, @tailwindcss/vite, class-variance-authority, clsx, tailwind-merge, tw-animate-css, ai, @ai-sdk/react, lucide-react
- Edited vite.config.ts: added @tailwindcss/vite plugin and @/ alias
- Edited tsconfig.json: added paths for @/*
- check:electron: 96/96 pass
- electron:smoke: passed

## Step 0.2 — Theme CSS, cn() util, components.json, shadcn button smoke
- Created styles/theme.css with Tailwind v4 directives and shadcn CSS variables
- Created src/lib/utils.ts with cn() helper
- Created components.json at repo root
- Manually wrote button.tsx (shadcn CLI can't pass -w to pnpm in workspace)
- Created _RevampSmoke.tsx rendering in App.tsx chat view
- Added class="dark" to index.html <html>
- Updated biome.json for Tailwind CSS directive support
- Fixed electron_renderer_styles.test.cts for theme.css import resolution
- Fixed revamp_setup.test.cts import regex
- format/lint/check: passed (`check:electron` 100/100 tests)
- electron:smoke: passed
- Screenshot: `plans/revamp-shots/step-0.2.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; smoke card visible, no `agent-browser errors` or console output.

## Step 0.3 — Prove AI Elements markdown renders in Electron
- Checked DeepWiki plus current shadcn/AI Elements docs after the noninteractive add hit two traps.
- Correct add command needs `--yes --overwrite`; `--yes` alone does not answer existing-file overwrite prompts.
- pnpm workspace root installs need `ignoreWorkspaceRootCheck: true` in `pnpm-workspace.yaml` because pnpm 11 reads non-auth project settings there.
- Added `@ai-elements/message` via shadcn and dependencies: `radix-ui`, `streamdown`, `@streamdown/cjk`, `@streamdown/code`, `@streamdown/math`, `@streamdown/mermaid`.
- Confirmed the official registry item targets root `components/ai-elements/message.tsx`; moved generated `message.tsx` into `desktop/electron/renderer/src/components/ai-elements/` and added a static test to keep it there.
- Added generated `button-group`, `separator`, and `tooltip` UI primitives; updated the generated button to the current `radix-ui` import style and removed the old `@radix-ui/react-slot` dependency.
- Rendered `MessageResponse` in `_RevampSmoke.tsx` with heading, list, and TypeScript fenced code block.
- format/lint/check: passed (`check:electron` 101/101 tests); build warns about large Streamdown/Shiki/Mermaid chunks.
- electron:smoke: passed
- Screenshot: `plans/revamp-shots/step-0.3.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; markdown heading/list and syntax-highlighted code block visible, no `agent-browser errors` or console output.

## Step 1.1 — Pure session to AI Elements adapter
- Added `src/lib/ai-elements-adapter.ts`, a pure mapper from existing renderer `Message[]` data into AI Elements-friendly transcript items.
- Maps `user`, `assistant`, and `error` chat roles into AI Elements `Message` props; `error` keeps its original role for later styling while rendering from the assistant side because AI Elements does not expose an `error` sender.
- Preserves tool messages with their current `state` and leaves the Step 1.4 TODO for final AI Elements tool-state mapping after `tool.tsx` is generated.
- Added `tests/revamp_adapter.test.cts` covering user/assistant/error role mapping and tool-message preservation.
- format/lint/check: passed (`check:electron` 103/103 tests); build still warns about large Streamdown/Shiki/Mermaid chunks from Step 0.3.
- electron:smoke: passed
- Screenshot: `plans/revamp-shots/step-1.1.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; existing smoke surface remains visible, no `agent-browser errors` or console output.

## Step 1.2 — Rebuild transcript on AI Elements conversation and message
- Added `@ai-elements/conversation`; the documented `--yes` command still prompted on existing `button.tsx`, so reran with `--yes --overwrite`.
- Added the generated `conversation.tsx` under `desktop/electron/renderer/src/components/ai-elements/` and kept the static setup test guarding AI Elements files against staying in root `components/`.
- Added `use-stick-to-bottom`, the dependency pulled by the AI Elements conversation registry item.
- Rebuilt `Transcript.tsx` to render AI Elements `Conversation`/`ConversationContent`, AI Elements `Message`/`MessageContent`, and the Step 1.1 adapter; kept the existing sanitized `renderMarkdown` path for assistant text until Step 1.3.
- Removed the temporary `_RevampSmoke.tsx` render and file, and adjusted transcript/layout CSS from virtualized rows to the conversation scroll container.
- format/lint/check: passed (`check:electron` 102/102 tests); build still warns about large Streamdown/Shiki/Mermaid chunks.
- electron:smoke: passed
- Screenshot: `plans/revamp-shots/step-1.2.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; empty chat surface renders without the smoke card, no stray conversation scroll button, no `agent-browser errors` or console output.

## Step 1.3 — Render assistant markdown through Streamdown
- Replaced transcript assistant markdown rendering with AI Elements `MessageResponse` (Streamdown) and removed `renderMarkdown`/`MarkdownText` from `Transcript.tsx`.
- Ran `rg -n "marked|dompurify|renderMarkdown|MarkdownText" desktop/electron/renderer/src`; remaining hits are only in the old `src/lib/markdown.ts` helper, so `marked`/`dompurify` deps stay for Phase 5 cleanup.
- Removed the old `.markdown` transcript CSS block and kept a small `.message-response` wrapper rule for transcript typography.
- Added a static setup test that guards `Transcript.tsx` against regressing back to `renderMarkdown`/`MarkdownText`.
- format/lint/check: passed (`check:electron` 103/103 tests); build still warns about large Streamdown/Shiki/Mermaid chunks, though the transcript bundle is smaller after dropping the old markdown path.
- electron:smoke: passed
- Screenshot: `plans/revamp-shots/step-1.3.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; local app remained disconnected/no-session, but the surface is stable with no `agent-browser errors` or console output.
