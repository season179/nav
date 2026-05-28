# TUI fullscreen — alt-screen substrate + mouse-wheel scroll

Switch nav's Ink TUI to fullscreen mode: alternate-screen substrate, SGR
mouse tracking, virtualized history pane driven by trackpad/mouse-wheel
only. Built fresh on `ink@6.8` + `react@19.2` + bun — **no vendoring**,
**no Ink fork**, **no copied code**. claude-code's TUI at
`/Users/season/Personal/claude-code-2.1.88/source/src/` is studied for
inspiration only; its renderer-integrated patterns don't port verbatim
to app-level work on vanilla Ink.

Driver: issue #374 follow-up. The current `HistoryRegion` (a) corrupts
the pane on multi-tool turns because the dynamic Ink area reflows every
streaming frame, and (b) gives no terminal scrollback because the
dynamic area fills the viewport. A bolted-on PgUp/PgDn/↑/↓ handler
conflicts with keys nav reserves for other UI.

Related memory: `MEMORY.md` → `reference-claude-code-tui` (study
reference, not a code source), `project-tui-parity` (target).

---

## Product framing

**Fullscreen is the only mode.** No `--no-fullscreen` flag, no config
opt-out, no auto-detection fallback. nav has exactly one user on one
class of setup (modern terminal + tmux with `set -g mouse on`). Dual
code paths to support hypothetical other setups = yak-shaving.

Accepted-by-fiat tradeoffs:
- No native terminal scrollback — in-app scroll is the only scroll.
- tmux mouse mode required; user enables it in their own tmux config.

---

## Architecture

### Building blocks (new, in `tui/src/ink-ext/`)

#### 1. `<AlternateScreen>` (`AlternateScreen.tsx`, ~80 LOC)

- Mount: writes `\x1b[?1049h\x1b[2J\x1b[H` via `useStdout`. With
  `mouseTracking={true}` also writes `\x1b[?1000h\x1b[?1006h`.
- Unmount: writes `\x1b[?1000l\x1b[?1006l\x1b[?1049l` (must disable
  both `?1000` and `?1006` per Codex round-3 catch).
- Cursor: **does not globally hide on mount.** Ink manages cursor
  visibility per-frame; the composer (`ink-text-input`) renders its
  own inverse-cell cursor. The exit path still writes `\x1b[?25h`
  on unmount as a belt-and-suspenders restore — harmless if Ink
  already left the cursor visible (Codex round-4: avoid
  unconditional hide; trust the composer).
- `signal-exit` is added as a **direct nav dependency** (not relying
  on Ink's transitive). Restores main screen + mouse off + cursor
  show on SIGINT and SIGTERM. We do **not** handle
  `uncaughtException` — a crashed process should crash visibly with
  its stack trace; swallowing it via cleanup hooks would mask bugs
  (Codex round-4 catch).
- Constrains children to `<Box height={rows} width={cols}
  flexShrink={0}>`. Vanilla Ink has no viewport concept; we enforce.
- Uses `useInsertionEffect` (not `useLayoutEffect`) so the enter
  sequence reaches the terminal *before* Ink's first render frame.
  Otherwise the first frame lands on the main screen and gets
  revealed at exit. This trick ports verbatim from claude-code — a
  React-effect-timing pattern, not a renderer-integration trick.

#### 2. Stdin proxy + mouse parser (`mouse.tsx`, ~300 LOC)

**Architecture per Codex round-3/4**: a custom `Readable` stream
passed to Ink's `render({ stdin: proxy })`. **Not a `data` listener on
`process.stdin`.**

Why: Ink 6.8 enables raw mode, attaches a `readable` listener, calls
`stdin.read()`, then pushes chunks through its own parser
(`tui/node_modules/ink/build/components/App.js:100`). A pre-render
`data` listener on `process.stdin` switches stdin into flowing mode
and consumes bytes before Ink's `readable` path sees them.
Re-emitting `stdin.emit('data', chunk)` does NOT put bytes back into
Ink's readable buffer. That shape would fail.

Implementation caveats (Codex round-4):
- Do **not** call `setEncoding` on real `process.stdin`. Parse raw
  bytes. Let the proxy handle Ink's `setEncoding('utf8')` when Ink
  asks for it.
- Proxy implements: `isTTY` (delegates), `setRawMode` (delegates),
  `ref`/`unref` (delegates), `setEncoding` (stores; proxy writes
  encoded strings when set), `_read` (no-op; we push data into it).
- Buffer partial SGR sequences across chunks with a short timeout
  (~25ms). Without the timeout, a plain `Esc` press or a malformed
  sequence sticks in the buffer forever and the user's Esc never
  reaches Ink.
- Only call `proxy.push(null)` when Ink is actually unmounting.
  Ending the proxy stream early triggers Ink's input-shutdown path
  in weird ways.
- Backpressure: ignore. Human typing/scrolling does not generate
  enough volume to matter.

Correct shape:

```
process.stdin ─[our 'data' listener]─► parser
                                       ├─► SGR mouse bytes ─► EventEmitter ─► React context
                                       └─► non-mouse bytes ─► proxy.push(chunk)
                                                              │
                                                              ▼
                                                         Ink render({stdin: proxy})
                                                              │
                                                              ▼
                                                         Ink's readable listener + parser
                                                              │
                                                              ▼
                                                          useInput()
```

Proxy stream:
- Extends `Readable` (object mode off; bytes only).
- Delegates `isTTY`, `setRawMode`, `ref`, `unref`, encoding to real
  `process.stdin`.
- Buffers any partial SGR sequence between chunks (the sequence may
  arrive split across two `data` events).
- Teardown: stop listening to `process.stdin`, flush any
  fully-parsed non-mouse bytes into proxy via final `proxy.push()`,
  then `proxy.push(null)` to end. This ordering is the only way to
  guarantee no lost keystrokes during shutdown.

Wired in `cli.tsx` (final shape — matches FS-05):
```typescript
const {proxy, mouseEvents} = createStdinProxy(process.stdin)
render(
  <AlternateScreen mouseTracking>
    <MouseEventProvider emitter={mouseEvents}>
      <App />
    </MouseEventProvider>
  </AlternateScreen>,
  { stdin: proxy, exitOnCtrlC: false }
)
```

Mouse events delivered through `MouseEventProvider` →
`useMouseEvents()` context. Wheel: `Cb & 64` flag; direction by
low bit. Surface as `{type: 'wheel', direction: 'up'|'down', ctrl,
shift, alt}`.

The parser stays dumb: it parses SGR mouse bytes and emits wheel
events into the EventEmitter unconditionally. **Overlay routing
happens in React land** — `useWheelScroll()` reads an
`overlayOpen` boolean from context and ignores wheel events when
it's true. Codex round-5 catch: do not filter at the parser; the
parser shouldn't know about app state. No `wheelOwner` ref API in
v1 (YAGNI — no overlay needs to scroll its own content today).

#### 3. `<ScrollViewport>` — real virtualization (`ScrollViewport.tsx`, ~400 LOC)

**Per Codex round-3 + user direction: real virtualization in v1.**
No "render all, slice afterward" — that pays React + Yoga cost for
every cell every frame and will be worse than today on long turns.

Strategy:
- **Mount only visible + overscan.** Default overscan = 5 rows
  above + below the viewport.
- **Height cache** keyed by `(message.id, viewport.width,
  message.contentVersion)`. `contentVersion` is a stable counter
  bumped whenever a message's text/structure changes (streaming
  updates).
- **Estimated heights** for not-yet-measured messages: coarse single
  default (e.g. 4 rows for all message types). On first mount, the
  cell is measured via `measureElement`, height cached, layout
  corrected next render. SPIKE-B proves this doesn't flicker.
  Don't tune a per-type table upfront — let measurement correct the
  estimate (Codex round-4 simplification).
- **scrollTop** is logical-row offset (not pixel). Cumulative
  heights from cache → which message range to mount.
- **stickyBottom** (default true) with 1-row tolerance: at bottom +
  new content → scrollTop advances; scrolled up + new content →
  scrollTop stays.
- **Resize**: width change invalidates the height cache (heights
  depend on wrap); rows change clamps scrollTop.
- **Height grow mid-stream**: streaming message's contentVersion
  bumps → cache evicts entry for that message → re-measure → if
  sticky, scrollTop advances by delta.

Public API:
```typescript
<ScrollViewport
  messages={messages}              // HistoryMessage[]
  renderMessage={fn}                // (msg) => ReactNode
  estimatedHeight={fn}              // (msg) => number
  scrollTop={scrollTop}             // controlled
  onScrollTopChange={fn}
  viewportHeight={rows}             // from useStdout
  stickyBottom={true}
  overscan={5}
/>
```

#### 4. `use-wheel-scroll.ts` (~80 LOC)

Hook owning `scrollTop` state. Subscribes to `useMouseEvents()`,
maps wheel events → scrollTop delta (N lines per event, N=3
default). Throttles to one render per ~16ms via `setTimeout`. Used
by `HistoryRegion`.

### Cross-cutting: Ctrl+C ownership

**Problem 1**: Ink's default `exitOnCtrlC: true` makes Ink exit
*before* app-level `useInput` sees Ctrl+C
(`tui/node_modules/ink/build/components/App.js:68`). The App handler
at `App.tsx:75` may never run today, and won't run in fullscreen
either unless we change this (Codex round-4 catch).

**Problem 2**: `App.tsx:254` does an async `for await` over
`client.streamMessage(...)`; `client.ts:316` only stops the owned
backend. `NavBackendClient.streamMessage` (`client.ts:265`) has no
AbortController. The events fetch (`client.ts:448`) has no abort
signal. Ctrl+C today either hangs or leaks.

**Solution** (acceptance criteria on FS-05):
1. Pass `exitOnCtrlC: false` to `render()` in `cli.tsx` so Ink
   defers Ctrl+C to the app.
2. App-level `useInput` catches Ctrl+C:
   - Calls `controller.abort()` on the active turn's
     `AbortController`.
   - Calls `client.close()` for backend cleanup.
   - Calls `useApp().exit()` to unmount Ink (which triggers
     `<AlternateScreen>` cleanup: alt-screen exit + mouse disable
     + cursor restore).
3. `NavBackendClient.streamMessage` accepts a `signal: AbortSignal`
   parameter and threads it into the RPC send call and the events
   fetch (`client.ts:448`).
4. The `streamMessage` invocation in `App.tsx` creates the
   per-turn `AbortController` and passes its signal.

This is the only place outside FS-01..03 that touches existing nav
code beyond `HistoryRegion` and `cli.tsx`.

### Where these plug in

In `cli.tsx` (the bun entry):
1. Build the stdin proxy + mouse EventEmitter (FS-02).
2. `render(<AlternateScreen mouseTracking><MouseEventProvider
   emitter={...}><App /></MouseEventProvider></AlternateScreen>, {
   stdin: proxy })`.

In `tui/src/regions/history/HistoryRegion.tsx`:
- Body becomes `<ScrollViewport>` driven by `useWheelScroll()`.
- **Delete the useInput PgUp/PgDn/↑/↓ handler at
  HistoryRegion.tsx:62-78.**

In `tui/src/preview.tsx:170`: update to the new `HistoryRegion` API.

### Out of scope

- Click + drag text selection.
- DECSTBM hardware scroll-region (vanilla Ink can't).
- Composer / `ink-text-input` rewrite (upstream Ink stays).
- `ink-testing-library` migration (no runtime swap).
- Any non-fullscreen fallback.

---

## Spikes (pre-PR, throwaway)

Spikes live in `tui/scratch/` (gitignored — see root `.gitignore`).
Output of each is appended to this plan as a "Spike confirmed"
paragraph before the corresponding stages are unblocked. **All
three spikes must pass before Wave 1.**

### SPIKE-A — Stdin proxy + Ink under bun + tmux

**Goal**: prove the proxy stdin approach delivers wheel events
without breaking Ink's keyboard input pipeline.

**Test app**: minimal Ink app with a `<TextInput>` + `useInput`
logging. `cli.tsx` builds proxy via `createStdinProxy(process.stdin)`
and passes it to `render(..., {stdin: proxy})`. Run under tmux with
`set -g mouse on`.

**Pass criteria** (all must hold):
- Typing → appears in TextInput (proxy delivers non-mouse bytes).
- Enter / Escape / Backspace / Ctrl+C → useInput fires with correct
  key info.
- Mouse wheel up/down → wheel events arrive in our EventEmitter; do
  NOT arrive in `useInput`.
- SGR split across two chunks → still parsed.
- Multiple SGR per chunk → all parsed.
- SGR interleaved with typing → both delivered correctly.
- Proxy stream's `isTTY`, `setRawMode`, `ref`, `unref` correctly
  delegate to real stdin (verify by checking Ink's raw-mode toggle
  reaches the terminal).
- Teardown: proxy.push(null), no lost keystrokes during shutdown.

**Fail mode**: if Ink's input pipeline breaks (typing nothing, or
wheel chars leak into TextInput), the plan is invalid — redesign.

### SPIKE-B — Virtualized ScrollViewport correctness + perf

**Goal**: prove real virtualization (mount-only-visible + cache +
overscan) renders correctly AND meets a perf gate that "render all"
cannot.

**Test app**: 200 mixed cells (short text, multi-line markdown,
tool-result block with 20-line stdout, file-changed cell).
`<ScrollViewport>` with overscan=5, height cache, estimated
heights.

**Correctness pass criteria**:
- All visible cells render correctly on first mount (no zero
  heights once cached).
- Streaming update (cell #25 grows from 3 → 10 lines): cache evicts
  for that message, re-measures, layout corrects without flicker.
- Resize (rows 40 → 25 → 40, cols 100 → 80 → 100): width change
  invalidates cache; row change clamps scrollTop.
- Wide-char content (CJK, emoji) measured correctly.
- **First-scroll over long unmeasured cells** (Codex round-5
  addition): include cells with realistic-worst-case
  height (40+ rows: long stdout tool-result, multi-screen
  markdown). Default estimate is 4 rows for all cells; scroll
  through these from initial mount and verify the viewport
  doesn't jump catastrophically as measurement corrects each
  cell. Tolerable: small layout shift on first appearance.
  Intolerable: scroll position appears to "skip ahead" by 30+
  rows because the estimate was wildly off and sticky-bottom
  followed. If this fails, raise the default estimate (e.g. to
  10) or add a per-type estimate after all.

**Perf pass criteria** (softened in Codex round-4 from strict
per-frame max to a distribution gate; a single outlier shouldn't
kill the gate when typical frames are fine):
- 200 cells, 50 wheel events (covering 50 scrollTop deltas).
- Frame render time measured via Ink's `onRender` callback.
- **Median ≤ 8ms** — typical wheel-tick is well under one 60fps
  budget.
- **p95 ≤ 16ms** — 95% of frames hit one 60fps budget.
- **Max ≤ 30ms** — outlier ceiling (catches catastrophic reflow,
  e.g. first-mount measure cascade).
- "Render all" alternative (control group): measure same workload
  without virtualization, log the comparison. Used to prove the
  virtualization is worth its complexity. If "render all" actually
  performs OK on this workload (it won't, but verify), document
  that.

**Fail mode**: if perf gate fails, virtualization design needs
adjustment (larger overscan? lazy cell components?) before FS-03
ships.

### SPIKE-C — End-to-end #374 prototype with automated residue check

**Goal**: prove the corruption bug disappears with the proposed
architecture before any PR is filed. **Automated check, not
visual.**

**Test app**: nav-shaped history (real `ToolCallCell`,
`ToolResultCell`, Markdown components) wrapped in
`<ScrollViewport>` inside `<AlternateScreen mouseTracking>` with
the mouse parser active. Synthetic backend that streams 6-10
fake tool calls with realistic output. Run in tmux.

**Automated residue detector** (Codex round-3 addition):
- After the run completes (composer back to "Enter send"), capture
  pane via `tmux capture-pane -p -S -500`.
- Assert: **all 6-10 expected final tool-call commands appear as
  exact lines** (proves cells are renderable, no overlap).
- Assert: **no row contains two `command:` substrings** (catches
  the specific #374 corruption pattern of two cell rows merged).
- Assert: **no row contains both `output` and `command:`** (same).
- Assert: **the final assistant message text appears verbatim**
  somewhere in the captured output.
- Assert: **row count equals row count predicted by the height
  cache** (no phantom rows from residue).
- Trackpad scroll smoke: inject SGR wheel-up sequences via tmux
  send-keys; assert visible viewport moves through prior cells
  (new lines appear at top, others scroll down).

**Fail mode**: if any residue assertion fails, the architecture
doesn't fix #374. Redesign before filing issues.

---

## Stages

Notation: `▶` sequential, `║` parallel, `[S]` strong-model.

Per Codex round-3 middle-ground guidance: split the previously-
"all-in cutover" into an adapter PR (test-only, not wired) +
runtime cutover PR. Same total work, smaller blast radius per PR,
no broken intermediate state on main.

### Wave 0 — Spikes (no PRs)

║ **SPIKE-A** · ║ **SPIKE-B** · ║ **SPIKE-C** — see above. All
three must pass before Wave 1.

### Wave 1 — Building blocks (3 PRs, parallel after spikes)

║ **FS-01** [S] — `<AlternateScreen>` standalone.
Acceptance:
- Mount + unmount sequences correct (capture stdout in test).
- `mouseTracking` on/off variants tested.
- Cursor: not globally hidden on mount; cursor-show emitted on
  unmount; double cleanup harmless.
- `signal-exit` added as direct nav dep (not transitive). SIGINT
  + SIGTERM + normal-exit + render-error paths restore main
  screen + mouse off + cursor show. **No `uncaughtException`
  handler** — let crashes crash with their stack.
- Children constrained to terminal rows × cols.
- `useInsertionEffect` ordering verified (no first-frame on main
  screen).
- `bun test` + `bun run typecheck` pass.

║ **FS-02** [S] — Stdin proxy + mouse parser + `MouseEventProvider`.
Acceptance: SPIKE-A pass criteria codified as test cases. Plus:
- Wheel scope context: simple `overlayOpen` boolean toggle drops
  wheel events when set (no `wheelOwner` ref API in v1; YAGNI).
- Teardown order tested (no lost keystrokes).
- `bun test` + `bun run typecheck` pass.

║ **FS-03** [S] — Virtualized `<ScrollViewport>` +
`use-wheel-scroll` hook. Acceptance: SPIKE-B pass criteria
codified, including the perf gate. Plus:
- API per spec above.
- 16ms wheel throttle verified.
- `bun test` + `bun run typecheck` pass.

### Wave 2 — Adapter (1 PR, no user-visible change)

▶ **FS-04** [S] — Create a new `VirtualHistoryRegion` component
that uses `<ScrollViewport>`. The new path is reachable in tests
and `preview.tsx` but **not wired in `cli.tsx` and not used by
`App.tsx`**. nav's behavior at this PR is unchanged.

Per Codex round-4: do **not** edit the existing
`HistoryRegion.tsx` here. `App.tsx:165` renders it directly, so
modifying it would change runtime behavior — contradicting the
"adapter, no user-visible change" intent. Keep the old component
intact; add the new one alongside.

Acceptance:
- New `tui/src/regions/history/VirtualHistoryRegion.tsx` exists
  using `<ScrollViewport>` + `useWheelScroll()`.
- New `VirtualHistoryRegion.test.tsx` covers the SPIKE-C residue
  assertions on canned history fixtures.
- `preview.tsx:170` updated to render `VirtualHistoryRegion`
  (preview is a test surface; no user-facing change).
- `App.tsx:165` and the existing `HistoryRegion.tsx` are
  **unchanged**. The old code path is exactly what main shipped
  before FS-04.
- `cli.tsx` unchanged.
- `bun test` + `bun run typecheck` pass.
- `bun run start` launches the existing (unchanged) UI; smoke
  test passes.

This stage exists so the runtime cutover PR (FS-05) is small and
purely about flipping the entry point + swapping the component in
`App.tsx`.

### Wave 3 — Runtime cutover (1 PR, the user-visible flip)

▶ **FS-05** [S] — Switch nav to fullscreen mode.

`cli.tsx`:
- Build stdin proxy via `createStdinProxy()`.
- Wrap App in
  `<AlternateScreen mouseTracking><MouseEventProvider>`.
- Pass `render(<…/>, { stdin: proxy, exitOnCtrlC: false })`.
  `exitOnCtrlC: false` is mandatory — without it Ink's default
  handler runs before App's `useInput` and Ctrl+C cleanup never
  fires (Codex round-4 catch;
  `tui/node_modules/ink/build/components/App.js:68`).

`App.tsx`:
- Replace `HistoryRegion` render at `App.tsx:165` with
  `VirtualHistoryRegion`.
- `streamMessage` creates a per-turn `AbortController`; signal
  passed to `session.sendMessage`.
- `useInput` at `App.tsx:75` owns Ctrl+C: aborts the active
  controller, calls `client.close()`, calls `useApp().exit()`.
- Wire wheel events via `useWheelScroll()`.

`tui/src/regions/history/HistoryRegion.tsx`:
- **Delete the useInput PgUp/PgDn/↑/↓ handler at
  HistoryRegion.tsx:62-78.** This file may otherwise be removed
  if no remaining caller references it after the App.tsx switch
  (verify with grep; remove only if no callers).

`tui/src/backend/client.ts`:
- `NavBackendClient.streamMessage` (line 265) accepts
  `signal: AbortSignal`. Thread into the events fetch (line 448).

Acceptance:
- `bun run start` enters alt-screen on launch; restores main
  screen on `/exit`, Ctrl+C, kill -TERM, normal exit.
- Trackpad scroll in tmux scrolls history; PgUp/PgDn/↑/↓ have no
  effect on history scroll.
- Streaming a turn, then Ctrl+C: App's `useInput` fires (Ink
  doesn't pre-empt because `exitOnCtrlC: false`); controller
  aborts; SSE fetch aborts; backend cleanup runs; alt-screen
  exits; mouse mode disables; cursor restored; no hung process.
- Manual check: composer cursor visibility feels right. We do
  **not** globally hide the cursor in alt-screen; Ink + the
  composer (`ink-text-input` uses an inverse cursor) manage cell
  rendering. If the manual check shows a stray block cursor in
  history, revisit `<AlternateScreen>` cursor handling. Otherwise
  leave Ink in charge.
- Existing `tmux-smoke-test.sh` passes (FS-06 extends).
- `bun test` + `bun run typecheck` pass.

### Wave 4 — Sign-off (1 PR)

▶ **FS-06** [S] — Extend `tmux-smoke-test.sh` with the SPIKE-C
residue detector codified as a smoke test, plus the synthetic
wheel-event injection.

Acceptance:
- `bash tui/scripts/tmux-smoke-test.sh` passes from a clean
  worktree.
- #374 closed with this PR's smoke output linked.
- `bun run typecheck` passes.

---

## Wave dependency graph

```
SPIKE-A ─┐
SPIKE-B ─┤
SPIKE-C ─┴─► FS-01 ─┐
                    │
              FS-02 ┼─► FS-04 ──► FS-05 ──► FS-06
                    │
              FS-03 ┘
```

6 PRs total + 3 throwaway spikes.

---

## Risks + mitigations

1. **Stdin proxy doesn't deliver bytes to Ink's readable buffer.**
   The whole project's cliff. Mitigation: SPIKE-A exhaustive
   criteria. If proxy doesn't work, fallback is to fork-and-patch
   Ink's input handling — at which point we revisit the vendor
   decision.

2. **`measureElement` returns stale/zero on first render → corruption
   returns.** Mitigation: SPIKE-B correctness pass criteria. Fallback
   = per-cell-type fixed heights.

3. **Virtualization complexity bugs (wrong overscan, off-by-one,
   sticky-bottom drift).** Mitigation: FS-03 has extensive unit
   tests; SPIKE-C is end-to-end proof.

4. **Vanilla Ink repaints viewport on every wheel tick (no
   DECSTBM).** Mitigation: 16ms throttle + virtualization keeps work
   per frame small. SPIKE-B perf gate enforces.

5. **Cursor restoration races with alt-screen exit.** Mitigation:
   FS-01 acceptance tests cover SIGINT, SIGTERM, normal exit, and
   render-error. (No `uncaughtException` handler — that path is
   intentionally absent per Codex round-4.) Double cleanup
   explicitly tested as harmless.

6. **Ctrl+C during streaming hangs.** Mitigation: AbortController in
   FS-05 acceptance. Today there's none — `client.ts:316` only
   handles backend close.

7. **tmux mouse mode not enabled.** Accepted by fiat.

8. **Native scrollback gone.** Accepted by fiat.

9. **claude-code patterns don't port cleanly.** Mitigation: this
   plan treats claude-code as inspiration only. The
   `useInsertionEffect` trick ports verbatim (React pattern); the
   renderer-integration tricks (DECSTBM, `setAltScreenActive`,
   scroll-region awareness) don't apply because vanilla Ink doesn't
   have them and doesn't need them in the same way.

10. **`signal-exit` accidental API.** Mitigation: add as direct dep
    in FS-01.

11. **Single 4-row estimated height is wildly off for long
    tool-result cells.** Symptom: first-scroll over an unmeasured
    40-row cell makes the viewport jump as the estimate corrects.
    Mitigation: SPIKE-B first-scroll test (above). If it fails,
    bump the default estimate or restore a per-type table — but
    don't tune speculatively. (Codex round-5 catch.)

---

## Open questions

1. **Wheel magnitude tuning.** Default `N=3` lines per wheel event,
   16ms throttle. Tune in FS-05 after manual testing.

2. **Modifier-keyed scroll.** Shift+wheel, Cmd+wheel as page-scroll?
   Deferred; solve in FS-05 if it feels right.

3. **Estimated cell heights.** Single default (4 rows) for all
   types in v1; measurement corrects on first mount. Revisit if
   SPIKE-B shows visible flicker on first-paint of long cells.

4. **Overlay wheel routing.** Default = dropped via `overlayOpen`
   boolean. Future overlay that wants to scroll its own content
   triggers the addition of a `wheelOwner` ref API at that point.
   Not in v1.

---

## Codex review history

- **Round 1 (2026-05-28)**: critiqued v1 (vendor the fork). Killed
  vendor approach due to provenance + maintenance cliff. User
  overrode the `--no-fullscreen` escape hatch recommendation.
- **Round 2 (2026-05-28)**: critiqued v2. Strengthened spikes,
  merged all-in cutover, caught `preview.tsx:170`, reframed
  claude-code as inspiration not port.
- **Round 3 (2026-05-28)**: critiqued v3. **Major architectural
  corrections**: stdin proxy (not `data` listener re-emit); real
  virtualization (not "render all + slice"); AbortController for
  Ctrl+C during streaming; direct `signal-exit` dep; mouse cleanup
  must disable both `?1000` and `?1006`; explicit cursor
  management; automated residue detector in SPIKE-C; perf gate in
  SPIKE-B; FS-04 split into adapter + cutover.
- **Round 4 (2026-05-28)**: critiqued v4. Verdict: "v4 is
  materially good. Fix the FS-04 path split, make App own Ctrl+C
  with exitOnCtrlC: false, soften the perf gate to p95/max, and
  execute." Material fixes incorporated into v5:
  - **FS-04 contradiction**: original v4 said "adapt
    HistoryRegion" but also "cli.tsx unchanged, old code path
    still runs." Both can't be true — `App.tsx:165` renders
    `HistoryRegion` directly. Fix: create a new
    `VirtualHistoryRegion` component in FS-04; leave old
    `HistoryRegion` untouched; swap in `App.tsx:165` in FS-05.
  - **Ctrl+C ownership**: `exitOnCtrlC: false` must be passed to
    `render()` or Ink's default handler runs before App's
    useInput. Cross-cutting section rewritten; FS-05 acceptance
    updated.
  - **Perf gate**: changed from strict max ≤ 16ms to a
    distribution (median ≤ 8ms / p95 ≤ 16ms / max ≤ 30ms) — one
    outlier shouldn't fail the gate when the typical case is
    fine.
  - **`uncaughtException` handler removed** from
    `<AlternateScreen>` — swallowing crashes via cleanup hooks
    masks bugs. Keep SIGINT + SIGTERM + normal-exit +
    render-error paths only.
  - **Cursor handling softened**: no global hide on mount; trust
    Ink + `ink-text-input` to manage the cursor. Restore-show on
    unmount as belt-and-suspenders.
  - **Estimated heights simplified**: single default (4 rows) for
    all types instead of a per-type table; measurement will
    correct.
  - **`wheelOwner` ref API removed** from v1; replaced by simple
    `overlayOpen` boolean.
  - **Stdin proxy implementation caveats** added: do not call
    `setEncoding` on real `process.stdin`; proxy must delegate
    `isTTY`/`setRawMode`/`ref`/`unref`/`setEncoding`; buffer
    partial SGR with ~25ms timeout; `push(null)` only on actual
    unmount.
- **Round 5 (2026-05-28)**: clean settle. Verdict: "v5 correctly
  translated the round-4 verdict. You're clear to start spikes."
  Six small cleanup notes applied:
  - Risk #5 still said FS-01 covers `uncaughtException` —
    corrected.
  - Method name: `NavBackendClient.streamMessage` (not
    `sendMessage`); cross-cutting + FS-05 updated.
  - Stale `render(<App />, {stdin: proxy})` snippet in
    architecture replaced with the full FS-05 wrapper +
    `exitOnCtrlC: false`.
  - `overlayOpen` filter moved from "parser layer" to React
    land — parser stays dumb, `useWheelScroll()` consumes
    `overlayOpen` from context.
  - `tui/scratch/` added to root `.gitignore`.
  - New risk #11: single 4-row estimated height may be wildly
    off for long tool-result cells. SPIKE-B first-scroll test
    covers it.
  - Spike order: SPIKE-A → SPIKE-B → SPIKE-C (kill-switch order:
    stdin proxy invalidates runtime plan; perf invalidates UI
    feel; residue proves #374 fix).
