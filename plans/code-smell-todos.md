# Code Smell TODOs

## Done

- [x] Reduce drift risk between `desktop/electron/preload.cts` and `desktop/electron/request-validation.cts`.
  - Added `tests/electron_preload_boundary.test.cts`: it loads the compiled
    `preload.cjs` with a stubbed `electron` module, captures the exposed `nav`
    API, and cross-checks every normalized IPC payload against
    `request-validation.cjs`. If the inlined copy drifts, the cross-checks fail.

- [x] Extract repeated Electron main readiness guards.
  - Added `requireBackendUrl()`, `requireMainWindow()`, `requireActiveSessionId()`
    in `desktop/electron/main.cts`; ~15 duplicated `backendUrl`/session/window
    checks now read as intent. The guards narrow the nullable module state and
    shadow `backendUrl`/`mainWindow` so existing shorthand call sites are unchanged.

- [x] Retire the old backend lock-recovery cleanup.
  - This item belonged to the removed pre-Flue backend. The current backend is a
    TypeScript Flue service, so there is no remaining lock-recovery follow-up in
    the active implementation.

- [x] Split renderer CSS by feature/component.
  - `styles.css` is now an `@import` manifest; rules live in
    `renderer/styles/{base,sidebar,layout,stacks,transcript,composer}.css`. The
    split is a contiguous line partition, so the bundled output is unchanged.
    `tests/electron_renderer_styles.test.cts` now resolves `@import`s, so its
    assertions match rules wherever they live.

- [x] Document/test runtime-loaded preload entry.
  - `tests/electron_preload_boundary.test.cts` actually `require`s the runtime
    preload entry and pins the `main.cts -> preload.cjs` wiring, so a rename or a
    "dead code" removal of either side trips a test instead of silently breaking
    the renderer bridge.

- [x] Make model selection per session (was process-global).
  - The active model + its renderer metadata now live on each `Session`
    (`model: Arc<RwLock<ActiveModel>>`, `model_info`). `SessionStore` keeps an
    immutable `default_model`/`default_model_info` template that new and resumed
    sessions start from. `run_turn` re-reads its session's model before each
    provider call (via the shared `Arc`), so a mid-run switch still lands.
  - `session.switchModel`/`session.switchThinking` now require a `sessionId`, and
    `replace_model`/`switch_model` mutate only that session. The IPC/preload/
    renderer layers thread the active session id through, validated at the
    preload boundary like every other session-scoped call.
  - `tests/session.rs::switching_one_session_model_leaves_other_sessions_untouched`
    and the updated `electron_backend_client` e2e prove a switch is isolated to
    one session and leaves the default untouched.

- [x] Retire the old model-module split follow-up.
  - This item belonged to the removed pre-Flue backend model layer. The current
    model catalog and provider selection live in `backend/src/model-catalog.ts`.

## Open (need a product/scope decision — investigated, not yet changed)

- [ ] Split the remaining large components into smaller focused units.
  - Candidates: `App.tsx` (873) extract hooks/subscriptions from rendering;
    `main.cts` (now smaller after the guard extraction) move IPC registration to
    its own module; backend control-plane handlers can be split further if they
    keep growing.
  - Deferred deliberately: these are large, churny diffs better landed on their
    own so they stay reviewable, rather than mixed into the mechanical cleanup.
