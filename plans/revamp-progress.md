# Revamp progress

## Step 0.1 — Add Tailwind + libs and wire the `@` alias
- pnpm add: tailwindcss, @tailwindcss/vite, class-variance-authority, clsx, tailwind-merge, tw-animate-css, ai, @ai-sdk/react, lucide-react
- Edited vite.config.ts: added @tailwindcss/vite plugin and @/ alias
- Edited tsconfig.json: added paths for @/*
- check:electron: 96/96 pass
- electron:smoke: passed
