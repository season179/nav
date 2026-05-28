# Session storage — execution sequence

Companion to [session-storage.md](./session-storage.md). Issues live at
`#326`–`#371` on `season179/nav`.

## How to read this

- `▶ #N PREFIX-XX` — **sequential**. Only this issue is in flight; everything
  after waits on it.
- `║ #N PREFIX-XX` — **parallel**. Every `║` line in the same wave can be
  picked up by a separate agent simultaneously.
- **[S]** = strong-model · **[W]** = weak-model.
- Each wave starts once *all* `▶` items from the previous waves have merged.
  Parallel `║` items only need their own listed deps; pick them up as soon as
  those land, don't wait for the whole wave to finish.

---

## Critical path (≈ 14 strong-model steps)

```
TYPES-01 → SES-00 → SES-01 → SES-02 → SES-06 → SES-05 → ART-01 → ENC-01
        → ENC-02a/b → SES-08a → SES-08b → SES-08c → CMP-00 → CMP-05a
        → CMP-05b → CMP-06b
```

Pruning (`CMP-01..04`) is **off** the critical path — it can ship in
parallel with summarisation once `CMP-00` lands.

---

## Wave 1 — Foundation crate (1 PR)

▶ **#326 TYPES-01** [S] — `nav-types` crate, prefixed IDs, row-struct skeletons.

*No blockers. Pick up first. Single-lane: touches workspace `Cargo.toml`.*

---

## Wave 2 — Trait scaffolding (1 PR)

▶ **#327 SES-00** [W] — `Encoder`/`Decoder` trait stubs + empty `SessionStore`.

*Depends on TYPES-01. Single-lane: touches `crates/nav-harness/src/lib.rs`.*

---

## Wave 3 — Store open (1 PR)

▶ **#328 SES-01** [S] — `SessionStore::open` + WAL pragmas + retry + checkpoint.

*Depends on SES-00. Single-lane: chooses `rusqlite` features in workspace.*

---

## Wave 4 — Schema (1 PR)

▶ **#329 SES-02** [S] — Declarative schema for `sessions/runs/turns/turn_parts` + reconciliation.

*Depends on SES-01. Single-lane: schema constant + migration code.*

---

## Wave 5 — Three parallel after SES-02

After `#329` merges, these three are independent (different files, no shared
state):

- ║ **#330 SES-06** [S] — Canonical `Turn`/`Part`/`TurnMeta` types.
- ║ **#332 SES-04** [S] — `sessions` + `runs` CRUD + cost arithmetic.
- ║ **#334 ART-01** [S] — `artifacts` table + blob filesystem store.

*All three need SES-02 only. Up to 3 agents.*

---

## Wave 6 — Parallel after Wave 5 items land

Pick each up the moment its specific deps merge:

- ║ **#331 SES-07** [W] — Per-variant `Part` round-trip fixtures. *Needs SES-06.*
- ║ **#333 SES-05** [S] — `turns` + `turn_parts` CRUD + append-in-tx + pagination. *Needs SES-02 + SES-06.*
- ║ **#335 ENC-01** [S] — `provider_payloads` journal + decode-status state machine. *Needs ART-01.*
- ║ **#336 ENC-02a** [S] — OpenAI Chat Completions **encoder**. *Needs SES-06 + SES-00.*

*Up to 4 agents.*

---

## Wave 7 — Parallel after Wave 6 items land

- ║ **#337 ENC-02b** [S] — OpenAI Chat Completions **decoder**. *Needs SES-06 + SES-00 + ENC-01.*
- ║ **#344 PROTO-01** [S] — Part-level delta protocol + TUI consumption. *Needs SES-05.*
- ║ **#354 FTS-01a** [S] — Text projection layer for `turn_parts`. *Needs SES-05.*

*Up to 3 agents. PROTO-01 and FTS-01a can run in parallel with the ENC chain.*

---

## Wave 8 — Encoder fan-out

- ║ **#338 ENC-04** [S] — Crash-window recovery for `pending` envelopes. *Needs ENC-01 + ENC-02b.*
- ║ **#352 ENC-09** [W] — OpenAI-compatible gateway fixtures. *Needs ENC-02b.*
- ║ **#339 SES-08a** [S] — Wire `RunLoop` to SQLite (canonical turns only). *Needs SES-04 + SES-05.*
- ║ **#355 FTS-01b** [S] — FTS5 virtual tables + triggers + anchored view. *Needs FTS-01a.*

*Up to 4 agents.*

---

## Wave 9 — Loop integration (sequential within)

These three must merge in order; each depends on the previous:

▶ **#340 SES-08b** [S] — Route encoder through trait + envelope journaling. *Needs SES-08a + ENC-02a/b + ENC-01.*

▶ **#341 SES-08c** [S] — Per-iteration transaction (turns + parts + provider_state + cost).

*Critical-path gate. Nothing past this point can start until #341 merges.*

---

## Wave 10 — The big fan-out (post-`SES-08c`)

Once `#341` merges, everything below depends only on `SES-08c` and can run
**fully in parallel**. This is the realistic 8–10 concurrent agents target.

### Dialects (6 wide)

- ║ **#345 ENC-06a** [S] — OpenAI Responses encoder. *(Highest priority of the three dialects per your call.)*
- ║ **#346 ENC-06b** [S] — OpenAI Responses decoder.
- ║ **#347 ENC-07a** [S] — ChatGPT/Codex subscription encoder.
- ║ **#348 ENC-07b** [S] — ChatGPT/Codex subscription decoder.
- ║ **#349 ENC-05a** [S] — Anthropic Messages encoder. *(Lowest priority.)*
- ║ **#350 ENC-05b** [S] — Anthropic Messages decoder.

### Loop polish (2 wide)

- ║ **#342 SES-09** [S] — Batch-flush streaming deltas + flush cursor.
- ║ **#343 SES-10** [S] — `data_dir` resolution + RPC handler wiring.

### Compaction foundation (1)

- ║ **#356 CMP-00** [S] — Replay projection / truncation harness.

### Polish (4 wide)

- ║ **#367 OPS-01** [S] — Fork session.
- ║ **#368 OPS-02a** [S] — Revert metadata + clear-on-continue.
- ║ **#370 OPS-03** [W] — Auto-title generation.
- ║ **#371 OPS-04** [W] — Session cost/token SSE surface.

---

## Wave 11 — After their specific deps land

- ║ **#351 ENC-08** [S] — Cross-provider replay test + `provider_state` invalidation. *Needs ENC-05b + ENC-06b.*
- ║ **#369 OPS-02b** [S] — Filesystem snapshot capture/restore. *Needs OPS-02a.*

After `CMP-00` (#356) lands, **4-wide pruning fan-out**:

- ║ **#357 CMP-01** [S] — Tool-result pruning via `compacted_at`.
- ║ **#358 CMP-02** [W] — Tool-result dedup by content hash.
- ║ **#359 CMP-03** [W] — Tool-call argument truncation. *(Also needs ART-01 — landed in Wave 5.)*
- ║ **#360 CMP-04** [W] — Old `Image` part stripping.

And in parallel:

- ║ **#361 CMP-05a** [S] — Summarisation: selection + marker + tail.

---

## Wave 12 — Summarisation chain + degraded replay

- ║ **#353 ENC-10** [S] — Degraded replay for unmappable old turns. *Needs ENC-08.*
- ║ **#362 CMP-05b** [S] — Summarisation: model call + template + incremental merge. *Needs CMP-05a.*
- ║ **#363 CMP-06a** [S] — Provider-agnostic context-limit error classifier. *Needs all four `ENC-*b` decoders (#337, #346, #348, #350).*

---

## Wave 13 — Overflow + breakers (after CMP-05b + CMP-06a)

- ║ **#364 CMP-06b** [S] — Overflow handler + media strip + replay user message.
- ║ **#365 CMP-07** [W] — Anti-thrashing breaker.
- ║ **#366 CMP-08** [S] — Summary validator + compaction failure breaker.

*Up to 3 agents.*

---

## Realistic concurrency budget

| Phase | Wide-ness | Notes |
|---|---|---|
| Waves 1–4 | 1 | Pure single-file foundation; cannot fan out. |
| Wave 5 | 3 | Three independent modules. |
| Wave 6–8 | 3–4 | Encoder/decoder + protocol/FTS lanes. |
| Wave 9 | 1 | Critical-path bottleneck (SES-08b → SES-08c). |
| **Wave 10** | **8–13** | Real fan-out — dialects (6) + loop polish (2) + CMP-00 (1) + polish (4). |
| Waves 11–13 | 3–6 | Compaction tail + breakers. |

**Peak concurrency** lands in Wave 10. Before then, expect 1–4 in flight.

---

## When to revisit this doc

- If a strong-model issue feels too big once implementation starts, split it
  and amend this file (mirrors the v1→v2 reshuffle Codex caught).
- If a "parallel" pair turns out to collide on a real file, demote one to
  sequential here and add a short reason.
- Re-check the critical path after Wave 9 lands — it's the most fragile guess.
