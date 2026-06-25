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
