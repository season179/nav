# Product

## Register

product

## Users

Developers running `nav`, an AI coding agent, through its desktop (Electron) chat client. They're in a focused work session: issuing instructions, watching the agent call tools, reading its responses. Often a long-running session with many turns.

## Product Purpose

A desktop front-end for conversing with the `nav` agent: a sidebar of sessions, a chat transcript with inline tool-activity lines, and a composer. Success is the interface staying out of the way so the user can read agent output and drive the next step fast.

## Brand Personality

Terminal-native, precise, a little bit hacker. Three words: sharp, focused, alive. It should feel like a power tool for someone who lives in the terminal, not a consumer chat app.

## Anti-references

- Generic SaaS chat UIs with rounded message bubbles and avatar labels (explicitly removed; sender is read from left/right alignment).
- Neon-soup cyberpunk: rainbow glow, glitch text, scanline overlays, repeating-gradient backgrounds. The aesthetic is committed and disciplined, not a costume.
- The warm cream/beige light theme this replaced.

## Design Principles

- **One signal.** A single cyan-green accent carries active, focus, and running states. Color means something; it is not decoration.
- **The tool disappears.** Dense, legible, fast. Personality lives in the chrome (palette, type, spacing), never in re-decorating the content.
- **Glyph over chrome.** Status and meaning come from monospace glyphs and alignment, not bubbles, stripes, or badges.
- **Alive, not busy.** Motion is reserved for state and focus (caret blink, focus glow, running pulse), 150-250ms, always with a reduced-motion fallback.

## Accessibility & Inclusion

- WCAG AA: body text >=4.5:1, large/UI text >=3:1 against its surface. Verified against the dark base.
- Error state never relies on the accent-vs-red hue alone (colorblind-safe): it also carries a distinct `✕` glyph and tinted surface.
- Every animation has a `prefers-reduced-motion: reduce` alternative (instant / static).
