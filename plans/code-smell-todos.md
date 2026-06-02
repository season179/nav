# Code Smell TODOs

- [ ] Reduce drift risk between `desktop/electron/preload.cts` and `desktop/electron/request-validation.cts`.
  - Preload duplicates validation because it runs sandboxed and cannot import project files.
  - Add direct preload-boundary tests or introduce a generated/shared validation source if practical.

- [ ] Split large modules/components into smaller focused units.
  - `src/model.rs` currently owns a lot of provider/adaptor behavior.
  - `desktop/electron/renderer/src/App.tsx` mixes app state, subscriptions, event handling, and rendering.
  - `desktop/electron/main.cts` mixes IPC registration, backend lifecycle, session orchestration, and smoke tracing.
  - `src/lib.rs` mixes HTTP/RPC routing, request parsing, and backend helpers.

- [ ] Extract repeated Electron main readiness guards.
  - Many IPC handlers in `desktop/electron/main.cts` repeat `backendUrl` / session / window checks.
  - Consider helpers such as `requireBackendUrl()`, `requireMainWindow()`, and `requireActiveSessionId()`.

- [ ] Revisit whether model selection should be global or per session.
  - `session.switchModel` and `session.switchThinking` currently do not take a session id.
  - `SessionStore` holds active model/model info globally, while the UI supports multiple live/background sessions.
  - Confirm intended product behavior and update naming/API if global is intentional.

- [ ] Decide how to handle poisoned Rust locks.
  - `src/session.rs`, `src/storage/mod.rs`, `src/agent.rs`, and `src/stack_store.rs` use many `.lock().unwrap()` / `.read().unwrap()` / `.write().unwrap()` calls.
  - If long-running reliability matters, add lock helpers that recover/log poison instead of panicking.

- [ ] Split renderer CSS by feature/component.
  - `desktop/electron/renderer/styles.css` is large and broad.
  - Consider extracting sidebar, composer, transcript, stacks, and layout styles.

- [ ] Document/test runtime-loaded preload entry.
  - `desktop/electron/preload.cts` is loaded by path from `desktop/electron/main.cts`, so import-graph tooling sees it as unreachable.
  - Keep this explicit in tests/docs so future dead-code cleanup does not remove it accidentally.
