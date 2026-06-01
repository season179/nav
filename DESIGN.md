# Design

Visual system for the `nav` desktop (Electron) client. Dark gray, terminal-native, single cyan-green accent. Scope: `desktop/electron/renderer/`.

## Theme

Dark gray, disciplined. A charcoal base with a small number of layered surfaces, one cyan-green accent for every active state, and system UI typography with mono reserved for identifiers, code, and tool activity. `color-scheme: dark` so native controls and scrollbars follow.

Color strategy: **Restrained**. There are only three color profiles: charcoal neutral, cyan-green state, and muted red error. Neutral gray carries structure, cyan-green marks active/focus/running state, and red appears only for errors.

## Color

Base profile values are OKLCH. Derived states use `color-mix()` so hover, border, tint, and selected states do not introduce new palettes.

| Token | Value | Role |
|---|---|---|
| `--bg` | `oklch(0.21 0.004 95)` | App background |
| `--sidebar-bg` | `oklch(0.28 0 195)` | Opaque sidebar base |
| `--sidebar-glass` | derived neutral alpha mix | Sidebar glass sheen and hover fills |
| `--sidebar-glass-strong` | derived neutral alpha mix | Raised sidebar control fill |
| `--sidebar-glass-border` | derived neutral alpha mix | Glass control border |
| `--sidebar-backdrop-filter` | `blur(18px) saturate(1.08)` | Sidebar glass blur |
| `--surface` | `oklch(0.29 0.016 264)` | Chat and composer surface |
| `--surface-raised` | `oklch(0.39 0.012 261)` | Inputs, menus, and cards |
| `--surface-muted` | derived neutral mix | Hover and secondary fills |
| `--border` | derived neutral mix | Hairline dividers, control borders |
| `--border-strong` | derived neutral mix | Emphasized borders |
| `--ink` | `oklch(0.92 0.004 95)` | Primary text |
| `--ink-dim` | `oklch(0.76 0.004 95)` | Secondary text, labels, placeholder |
| `--ink-faint` | `oklch(0.58 0.004 95)` | Non-essential metadata |
| `--active` | `oklch(0.72 0.08 176)` | Active, focus, running state |
| `--active-dim` | derived accent mix | Quieter running state |
| `--active-tint` | derived accent mix | Selected backgrounds and focus rings |
| `--active-border` | derived accent mix | Active borders |
| `--danger` | `oklch(0.66 0.13 25)` | Error text and glyph |
| `--danger-tint` | derived error mix | Error surface |

Contrast: `--ink` and `--ink-dim` stay readable on the dark base. The active color is strong enough for state but is not used as body text. Error rows carry both red color and a `✕` glyph.

## Typography

One system sans family for UI, plus a mono stack for identifiers, code, tool rows, and IDs. No bundled fonts, so Electron stays offline/CSP-safe.

Sans: `ui-sans-serif, -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", sans-serif`

Mono: `ui-monospace, "SF Mono", SFMono-Regular, "JetBrains Mono", Menlo, Consolas, "Liberation Mono", monospace`

- Section labels: 10.5px / 700, uppercase, `0.06em` tracking, `--ink-faint`.
- Body messages: 14.5px / 1.62, `--ink`. Compact but legible for chat-length turns.
- Tool lines and data: 12.5px mono, `--ink-dim` detail, `--active` state glyph/name.
- Fixed rem/px scale (product register), not fluid clamp.

## Components

- **Messages**: flat, no bubbles, no sender labels. Sender read from alignment: assistant left, user right. Both `--ink` for legibility. New lines fade+rise in (160ms, reduced-motion: instant).
- **Tool line**: compact mono row on `--surface-muted` with a state glyph: running uses `--active-dim`, done uses `--active`, failed uses `--danger`. Failed row gets `--danger-tint` and `--danger` border.
- **Composer input**: `--surface-raised`, active `caret-color`, focus gets `--active-border` and a subtle `--active-tint` ring.
- **Session mode trigger**: fixed-width pill so switching Local/Worktree does not shift composer metadata.
- **Send button**: neutral gray circle; it remains a primary action by placement, not by introducing another accent fill.
- **Sidebar**: opaque charcoal base with a restrained glass treatment: subtle highlight, blur, and translucent neutral fills. The glass effect is decorative; contrast still comes from the solid base.
- **New thread**: raised neutral control; hover uses the same neutral surface ramp.
- **Session item**: `--ink-dim`; hover raises surface; current = `--active-tint` background + `--active-border` border.
- **Scrollbar**: native behavior kept, thumb uses the neutral border ramp.

## Motion

- Focus glow / hovers / button states: 150ms ease-out.
- Tool running pulse: 1.4s ease-in-out.
- New message entrance: 160ms fade + 4px rise.
- All gated behind `@media (prefers-reduced-motion: reduce)` -> static/instant.

## Z-index scale

Semantic, no magic numbers: `--z-base: 0`, `--z-sticky: 10`, `--z-overlay: 100` (reserved for future modals/toasts).
