# Design

Visual system for the `nav` desktop (Electron) client. Dark, terminal-native, single phosphor-green accent. Scope: `desktop/electron/renderer/`.

## Theme

Dark cyberpunk, disciplined. A near-black green-tinted base with layered surfaces, one phosphor-green accent for every active state, a monospace type system throughout. `color-scheme: dark` so native controls and scrollbars follow.

Color strategy: **Committed** — one saturated color (phosphor) owns identity and all interactive state; everything else is a tinted neutral ramp. Second hue (terminal red) appears only for errors, by semantic necessity.

## Color

All values OKLCH. Exposed as CSS custom properties on `:root`.

| Token | OKLCH | Role |
|---|---|---|
| `--bg` | `oklch(0.15 0.012 158)` | App / deepest surface (sidebar) |
| `--surface` | `oklch(0.185 0.014 158)` | Chat + composer surface |
| `--surface-raised` | `oklch(0.23 0.016 158)` | Inputs, hover, tool lines |
| `--border` | `oklch(0.30 0.018 158)` | Hairline dividers, control borders |
| `--border-strong` | `oklch(0.40 0.03 158)` | Emphasized / focused borders |
| `--ink` | `oklch(0.92 0.025 150)` | Primary text |
| `--ink-dim` | `oklch(0.72 0.022 150)` | Secondary text, labels, placeholder |
| `--ink-faint` | `oklch(0.56 0.018 150)` | Decorative / non-essential only |
| `--primary` | `oklch(0.84 0.20 150)` | Phosphor accent: active state, primary action, focus |
| `--primary-dim` | `oklch(0.70 0.15 150)` | Quieter accent (running, hover hints) |
| `--primary-tint` | `color-mix(in oklab, var(--primary) 14%, transparent)` | Selected / active backgrounds |
| `--danger` | `oklch(0.70 0.18 25)` | Error text + glyph (errors also carry a `✕`) |
| `--danger-tint` | `color-mix(in oklab, var(--danger) 14%, transparent)` | Error surface |

Contrast: `--ink` on `--bg` ≈ 14:1; `--ink-dim` on `--bg` ≈ 6:1 (placeholder/labels pass AA); `--primary` text on `--bg` ≈ 11:1; send button uses `--bg` text on `--primary` fill (high contrast).

## Typography

One monospace family in multiple weights — the terminal voice, and it dodges the multi-font "indecision" tell. System stack, zero bundled fonts (offline/CSP-safe in Electron):

`ui-monospace, "SF Mono", SFMono-Regular, "JetBrains Mono", Menlo, Consolas, "Liberation Mono", monospace`

- Brand `nav`: 18px / 700, phosphor, with a blinking block caret `▊` (`::after`, reduced-motion: steady).
- Section labels: 11px / 700, uppercase, `0.08em` tracking, `--ink-dim`.
- Body (messages): 13.5px / 1.6, `--ink`. Compact but legible for chat-length turns.
- Tool lines / data: 12.5px, `--ink-dim` detail, phosphor name.
- Fixed rem/px scale (product register), not fluid clamp.

## Components

- **Messages** — flat, no bubbles, no sender labels. Sender read from alignment: assistant left, user right. Both `--ink` for legibility. New lines fade+rise in (160ms, reduced-motion: instant).
- **Tool line** — compact mono row on `--surface-raised` with a state glyph: running `▸` (`--primary-dim`, soft pulse), done `●` (`--primary`), failed `✕` (`--danger`). Failed row gets `--danger-tint` + `--danger` border.
- **Composer input** — `--surface-raised`, phosphor `caret-color`, focus → `--border-strong` + phosphor glow ring (`box-shadow`).
- **Send button** — primary action: phosphor fill, `--bg` text, glow intensifies on hover.
- **New chat** — phosphor outline; fills to tint + brightens on hover.
- **Session item** — `--ink-dim`; hover raises surface; current = `--primary-tint` bg + full `--border-strong` + `--primary` text (no side-stripe).
- **Scrollbar** — thin, themed via `::-webkit-scrollbar` (native behavior kept), phosphor-dim thumb on hover.

## Motion

- Focus glow / hovers / button states: 150ms ease-out.
- Brand caret blink: 1.1s step. Tool running pulse: 1.4s ease-in-out.
- New message entrance: 160ms fade + 4px rise.
- All gated behind `@media (prefers-reduced-motion: reduce)` → static/instant.

## Z-index scale

Semantic, no magic numbers: `--z-base: 0`, `--z-sticky: 10`, `--z-overlay: 100` (reserved for future modals/toasts).
