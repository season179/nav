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

## Step 1.4 — Render tool calls through AI Elements Tool
- Checked DeepWiki plus current AI Elements/shadcn docs before wiring this step. DeepWiki confirmed `shadcn add` needs `--yes --overwrite` for noninteractive overwrites; the official AI Elements Tool docs define the accepted states as `input-streaming`, `input-available`, `approval-requested`, `approval-responded`, `output-available`, `output-error`, and `output-denied`.
- Added `@ai-elements/tool` with `pnpm dlx shadcn@latest add @ai-elements/tool -c . --yes --overwrite`; relocated generated `tool.tsx` and `code-block.tsx` into renderer src, and added generated `badge`, `collapsible`, and `select` UI primitives plus the direct `shiki` dependency.
- Mapped local tool states into AI Elements states: `running` -> `input-available`, `done`/`completed` -> `output-available`, `failed` -> `output-error`, unknown -> `input-streaming`.
- Rebuilt transcript tool rows on `Tool`, `ToolHeader`, `ToolInput`, and `ToolOutput`; removed the old custom tool glyph/preview CSS and the stale reduced-motion selector.
- Added/updated static tests for generated AI Elements relocation and adapter coverage for running/done/failed tool lifecycle mapping.
- format/lint/check: passed (`check:electron` 103/103 tests when rerun unsandboxed; sandboxed run failed only on known `127.0.0.1` listen `EPERM`); build still warns about large Streamdown/Shiki/Mermaid chunks, now with an additional direct Shiki tool-code path.
- electron:smoke: passed when rerun unsandboxed (sandboxed Electron launch aborted with `SIGABRT`).
- Screenshot: `plans/revamp-shots/step-1.4.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; local app remained disconnected/no-session so no live tool row was visible, but the surface is stable with no `agent-browser errors` or console output.

## Step 1.5 — Reasoning component probe
- Skipped: `rg -n "reasoning|thinking|thought" desktop/electron/renderer/src/types.ts desktop/electron/renderer/src/lib` found only model thinking-level settings, not streamed reasoning/thought transcript content.

## Step 2.1 — Rebuild composer input on AI Elements PromptInput
- Checked DeepWiki plus current shadcn/AI Elements docs first: DeepWiki only confirmed the shadcn overwrite behavior, while the official Prompt Input docs and generated file showed the real exports are `PromptInputFooter`/`PromptInputTools` rather than the plan's guessed `PromptInputToolbar`.
- Added `@ai-elements/prompt-input` with `pnpm dlx shadcn@latest add @ai-elements/prompt-input -c . --yes --overwrite`, relocated the generated `prompt-input.tsx` into renderer src, and added its generated UI primitives plus `cmdk`/`nanoid`.
- Rebuilt the composer text area/send shell on `PromptInput`, `PromptInputTextarea`, `PromptInputFooter`, `PromptInputTools`, and `PromptInputSubmit`; kept the TanStack form validation/draft/onSend path and the separate Stop button so running sessions retain their existing behavior.
- Removed the old manual Enter key handling and textarea autosize code; `PromptInputTextarea` now owns Enter-to-submit, Shift+Enter newline, IME composition handling, and field-sizing growth.
- Added/updated static tests for AI Elements relocation and PromptInput usage.
- format/lint/check: passed (`check:electron` 104/104 tests when run unsandboxed; sandboxed local-listener runs are still not reliable in this repo).
- electron:smoke: passed when run unsandboxed.
- Screenshot: `plans/revamp-shots/step-2.1.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; local app remained disconnected/no-session, but the new prompt shell is visible and stable with no `agent-browser errors` or console output.

## Step 2.2 — Model selector and context for composer metadata
- Checked DeepWiki plus current AI Elements docs first. DeepWiki could not provide the generated exports for `@ai-elements/model-selector`/`@ai-elements/context`, but confirmed the noninteractive shadcn overwrite path; the official docs show `ModelSelector*` command-dialog parts and `Context*` token/context-window parts.
- Added `@ai-elements/model-selector` and `@ai-elements/context` with `pnpm dlx shadcn@latest add @ai-elements/model-selector @ai-elements/context -c . --yes --overwrite`; relocated generated `model-selector.tsx` and `context.tsx` into renderer src, and added generated `progress.tsx` plus `tokenlens`.
- Replaced the custom composer `ModelMenu` popover/search with AI Elements `ModelSelector`, preserving the existing model list, selected-model check, provider grouping, and `onModelChange` callback.
- Replaced the plain token text with AI Elements `Context`/`ContextTrigger`/`ContextContentHeader`, while keeping the visible `used/contextWindow` text because the local model info API does not expose input/output/reasoning token breakdowns.
- Removed dead custom model popover/search CSS and added static setup tests for the new generated components and composer metadata wiring.
- format/lint/check: passed (`check:electron` 105/105 tests when run unsandboxed).
- electron:smoke: passed when run unsandboxed.
- Screenshot: `plans/revamp-shots/step-2.2.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; local app remained disconnected/no-session after a longer wait, so the model selector and token context could not be exercised visually, but the surface is stable with no `agent-browser errors` or console output.

## Step 2.3 — Thinking and session-mode menus on shadcn Select
- Did not re-add `select`; the shadcn `select.tsx` primitive was already generated in Step 1.4 by the AI Elements tool dependency.
- Replaced the custom `ThinkingMenu` and `SessionModeMenu` popover/menu implementations with shadcn `Select`, preserving the existing option lists and callback paths.
- Removed the dead custom thinking/session menu CSS and added a static setup test guarding against the old manual menu helpers/selectors returning.
- format/lint/check: passed (`check:electron` 106/106 tests when run unsandboxed).
- electron:smoke: passed when run unsandboxed.
- Screenshot: `plans/revamp-shots/step-2.3.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; local app remained disconnected/no-session, so only the disabled session-mode Select trigger was visible, but the surface is stable with no `agent-browser errors` or console output.

## Step 3.1 — Session toolbar tabs on shadcn Tabs
- Checked DeepWiki plus current shadcn Tabs/CLI docs first; both confirmed the controlled `Tabs`/`TabsList`/`TabsTrigger` shape and the `--yes --overwrite` CLI path for noninteractive adds.
- Added `tabs.tsx` with `pnpm dlx shadcn@latest add tabs -c . --yes --overwrite`.
- Replaced the custom thread-view `nav` buttons in `SessionToolbar` with controlled shadcn `Tabs`, preserving the existing `onSelectView` route callback and disabled states for settings/stacks.
- Moved the pill styling onto the generated `TabsList`, switched active styling to Radix `data-state="active"`, and added a static setup test guarding against the old `aria-current` button toolbar returning.
- format/lint/check: passed (`check:electron` 107/107 tests when run unsandboxed).
- electron:smoke: passed when run unsandboxed.
- Screenshot: `plans/revamp-shots/step-3.1.png` captured from Electron CDP target
  `file:///Users/season/Personal/nav/desktop/electron/renderer/dist/index.html#/chat`; local app remained disconnected/no-session, but the toolbar now snapshots as a real `tablist` with no `agent-browser errors` or console output.
