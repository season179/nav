# Codex Long-Session Compaction — Learnings

Source-grounded study of how the [OpenAI Codex Rust client](https://github.com/openai/codex) handles long sessions and context compaction. Read alongside `long-session-compaction-prd.md` — this is the implementation reference for that PRD.

All citations are from `~/Personal/codex/codex-rs/` at the `main` branch checkout I read.

## TL;DR

- Compaction is a **first-class session lifecycle event**, not an emergency trim. It is a real turn that asks the model to write a handoff for "another LLM", then atomically replaces the model-visible history with `[recent user messages] + [summary]`.
- Triggered three ways: **manual** (`/compact`), **automatic in-loop** (after a sampling response, before the next one, when a follow-up is needed and tokens crossed the limit), and a context-window-overflow **fallback inside the compaction turn itself**.
- The auto-compaction check fires **after every sampling response**, never *before* the first sampling of a freshly submitted user turn. Concretely: when a user types a prompt, codex sends it as-is without any pre-flight token check — the new prompt is appended to history and shipped. Only once that sampling completes does codex check `needs_follow_up && token_limit_reached`; if true, it compacts before the next sampling iteration. So there is **no "before I send your new prompt, let me compact first" path** — an oversized fresh prompt has to rely on the in-compaction overflow fallback or the provider's overflow error. (This is the key gap vs PRD story #17.)
- Pre-turn/manual compaction throws away the initial context block (because the next regular turn re-injects it). Mid-turn compaction re-injects the initial context immediately before the last user message, because the model is trained to see the summary as the last item.
- Persistence is rollout-based, not SQLite-table-based: a `RolloutItem::Compacted { message, replacement_history }` row is appended. On resume, the rollout is replayed sequentially, so the compaction row naturally shadows everything before it.
- The compaction turn is **non-steerable** — input submitted during compaction is rejected with `SteerInputError::ActiveTurnNotSteerable { turn_kind: NonSteerableTurnKind::Compact }`. Pending input is queued and drained after compaction completes.
- Tool calls/results from before compaction are **dropped**. Only user messages are carried forward. Encrypted reasoning / `previous_response_id` are not preserved across the boundary.

---

## 1. File map

The core compaction logic lives in **`codex-rs/core/src/`**:

| File | Role |
|---|---|
| `compact.rs` | Local (inline) compaction: the main loop, summary build, replacement history. 584 lines. |
| `compact_remote.rs` / `compact_remote_v2.rs` | Remote compaction via a provider's `/compact` endpoint (e.g., the Responses-API-native path). Used when `provider.supports_remote_compaction()`. |
| `compact_tests.rs` | Inline unit tests. |
| `tasks/compact.rs` | The `SessionTask` impl that routes `/compact` to local vs remote. Only 54 lines. |
| `session/turn.rs` | Where auto-compaction is *triggered* (post-sampling check, mid-turn). |
| `templates/compact/prompt.md` | The compaction prompt text. |
| `templates/compact/summary_prefix.md` | The "another LLM produced this summary" preamble injected ahead of the model's output. |
| `tests/suite/compact*.rs` | Integration tests. |
| `tui/src/slash_command.rs` + `tui/src/chatwidget/slash_dispatch.rs` | `/compact` slash command registration and dispatch. |

---

## 2. The compaction prompt (verbatim)

**`core/templates/compact/prompt.md`** (sent to the model as the compaction turn's user input):

```
You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.

Include:
- Current progress and key decisions made
- Important context, constraints, or user preferences
- What remains to be done (clear next steps)
- Any critical data, examples, or references needed to continue

Be concise, structured, and focused on helping the next LLM seamlessly continue the work.
```

**`core/templates/compact/summary_prefix.md`** (prepended to the model's output before it's inserted into history):

```
Another language model started to solve this problem and produced a summary of its thinking process. You also have access to the state of the tools that were used by that language model. Use this to build on the work that has already been done and avoid duplicating work. Here is the summary produced by the other language model, use the information in this summary to assist with your own analysis:
```

Both are **static** strings (`include_str!`) — there is no parametric templating. They are loaded at `compact.rs:46-47`:

```rust
pub const SUMMARIZATION_PROMPT: &str = include_str!("../templates/compact/prompt.md");
pub const SUMMARY_PREFIX:       &str = include_str!("../templates/compact/summary_prefix.md");
const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;
```

The prompt is submitted as a `UserInput::Text` (role = `"user"`) — see `run_inline_auto_compact_task` at `compact.rs:69-94`.

---

## 3. Triggers — three paths

### 3a. Manual `/compact`

Slash command registered in `tui/src/slash_command.rs:85`:

```rust
SlashCommand::Compact => "summarize conversation to prevent hitting the context limit",
```

Dispatched in `tui/src/chatwidget/slash_dispatch.rs:188` (and `tui/src/app/thread_routing.rs:631` for `AppCommand::Compact`) into the `SessionTask` system. Entry point in `core/src/tasks/compact.rs:13-54`:

```rust
async fn run(self, session, ctx, input, _cancellation_token) -> Option<String> {
    let session = session.clone_session();
    let _ = if crate::compact::should_use_remote_compact_task(ctx.provider.info()) {
        crate::compact_remote::run_remote_compact_task(session.clone(), ctx).await
    } else {
        crate::compact::run_compact_task(session.clone(), ctx, input).await
    };
    None
}
```

Manual compaction always uses `InitialContextInjection::DoNotInject`, `CompactionTrigger::Manual`, `CompactionReason::UserRequested`, `CompactionPhase::StandaloneTurn` (`compact.rs:108-116`).

### 3b. Automatic, in-loop (between sampling iterations)

A note on the name: codex's analytics enum has `CompactionPhase::{StandaloneTurn, PreTurn, MidTurn}`, and only `MidTurn` is actually invoked from the turn loop in current code. "Mid-turn" here is codex's terminology for **between two sampling iterations within the same agent turn** — *not* "in the middle of a single sampling response." There is no separate `PreTurn` path firing before the *first* sampling of a freshly submitted user prompt; the check below is the only auto path.

The auto-compact decision sits **inside the turn-execution loop**, *after* a sampling response completes. From `core/src/session/turn.rs:300-359`:

```rust
let token_status = auto_compact_token_status(sess.as_ref(), turn_context.as_ref()).await;
let token_limit_reached = token_status.token_limit_reached;
// ...trace...
if token_limit_reached && needs_follow_up {
    let reset_client_session = run_auto_compact(
        &sess,
        &turn_context,
        &mut client_session,
        InitialContextInjection::BeforeLastUserMessage,
        CompactionReason::ContextLimit,
        CompactionPhase::MidTurn,
    ).await?;
    if reset_client_session { client_session.reset_websocket_session(); }
    can_drain_pending_input = !model_needs_follow_up;
    continue;  // resume the turn loop with new compacted history
}
```

**Key nuance**: the check is gated on `needs_follow_up` (the model still has tool calls to run, or pending user input is queued). So:

- A fresh user prompt is **always sent first, without a token check.** If it fits and the model returns a final answer in one sampling, no auto-compaction ever happens.
- Auto-compaction only kicks in at the *boundary between sampling iterations*: after sampling N completes, if sampling N+1 is needed and the limit is now crossed, compact before issuing sampling N+1.
- The smallest blast radius of an auto-compaction is therefore a tool-call follow-up, never a brand-new user prompt.

### 3c. In-compaction overflow fallback

When the compaction turn itself overflows the context window, `compact.rs:223-232` trims the oldest history item and retries:

```rust
Err(e @ CodexErr::ContextWindowExceeded) => {
    if turn_input_len > 1 {
        error!("Context window exceeded while compacting; removing oldest history item.");
        history.remove_first_item();
        retries = 0;
        continue;
    }
    // single-item history: nothing left to trim → propagate error
}
```

The comment notes the trim is from the **beginning** to preserve prefix cache and keep recent messages intact. This replaces the older "drop one tool-call pair on overflow" pattern that nav still has as its primary recovery.

---

## 4. Token accounting & the threshold

The decision lives in `core/src/session/turn.rs:655-700`, in `auto_compact_token_status()`:

```rust
let active_context_tokens = sess.get_total_token_usage().await;
let (auto_compact_scope_tokens, auto_compact_scope_limit, full_context_window_limit) =
    match turn_context.config.model_auto_compact_token_limit_scope {
        AutoCompactTokenLimitScope::Total => (
            active_context_tokens,
            turn_context.model_info.auto_compact_token_limit().unwrap_or(i64::MAX),
            None,
        ),
        AutoCompactTokenLimitScope::BodyAfterPrefix => {
            let window = sess.auto_compact_window_snapshot().await;
            // measures growth since the last compaction window's prefill baseline
        }
    };
let token_limit_reached =
    auto_compact_scope_tokens >= auto_compact_scope_limit
    || full_context_window_limit_reached;
```

Two scope modes (`config_types::AutoCompactTokenLimitScope`):

- **`Total`** — absolute: total active context tokens ≥ `model_info.auto_compact_token_limit()`. Simple and what you'd reach for first.
- **`BodyAfterPrefix`** — relative: tracks growth since the prefill baseline of the *current* auto-compact window. After a compaction, a fresh window starts (`state.start_next_auto_compact_window()` is called inside `replace_compacted_history`). This is the more sophisticated mode — it triggers when *new* content since the last compaction exceeds a budget, rather than measuring the cumulative total. Useful when the compacted summary itself is large.

The actual `auto_compact_token_limit` is sourced from per-model metadata (`model_info.auto_compact_token_limit()`) and overridable through nav settings or CLI (`turn_context.config.model_auto_compact_token_limit`). Token counting is done in `core/src/context_manager/history.rs:309-327` — it sums the last server-reported `total_tokens` with an estimate for items added after that response.

---

## 5. Replacement history — what survives

Construction lives in `core/src/compact.rs:465-529`, `build_compacted_history` → `build_compacted_history_with_limit`:

1. **Walk user messages backwards**, accumulating up to `COMPACT_USER_MESSAGE_MAX_TOKENS` (20_000). Truncate the boundary message if it doesn't fit. Reverse to chronological order.
2. **Append each surviving user message** as a `ResponseItem::Message { role: "user", content: InputText { ... } }`.
3. **Append the summary** as one final `role: "user"` message, prefixed with `SUMMARY_PREFIX + "\n"`.

A critical detail at `compact.rs:404-406` — `is_summary_message()` checks for the `SUMMARY_PREFIX` so prior summary messages are filtered out of the "user messages to carry forward" list. **Compaction summaries never get re-summarized as if they were real user input.**

**What is dropped:**

- Assistant messages from before compaction (except as captured in the summary text).
- Tool calls and tool results (`function_call` / `function_call_output`).
- Reasoning items, including any encrypted reasoning blobs.
- Provider continuation handles (`previous_response_id`). The summary is plain text, not a continuation token.

**What is added back for mid-turn compaction only:**

For `InitialContextInjection::BeforeLastUserMessage`, the canonical initial context block is re-injected immediately *before* the last real user message (or before the summary if no real user messages survived) — see `insert_initial_context_before_last_real_user_or_summary` at `compact.rs:418-463`. The doc comment at `compact.rs:50-58` is worth quoting:

> Pre-turn/manual compaction variants use `DoNotInject`: they replace history with a summary and clear `reference_context_item`, so the next regular turn will fully reinject initial context after compaction.
>
> Mid-turn compaction must use `BeforeLastUserMessage` because the model is trained to see the compaction summary as the last item in history after mid-turn compaction; we therefore inject initial context into the replacement history just above the last real user message.

This is a load-bearing distinction — the model has been fine-tuned to expect the summary at the tail.

---

## 6. Non-steerable execution & queueing

Steering input into a compaction task is explicitly refused (`session/mod.rs:3182-3186`):

```rust
Some(crate::state::TaskKind::Compact) => {
    return Err(SteerInputError::ActiveTurnNotSteerable {
        turn_kind: NonSteerableTurnKind::Compact,
    });
}
```

While compaction is running, pending user input remains queued in `sess.input_queue`. The pending-input drain is gated by `can_drain_pending_input` in the turn loop (`turn.rs:237`, set to `true` only after a normal sampling response, *not* during the compact branch). When compaction completes, the queue drains naturally on the next loop iteration.

The compaction turn does **not** disable tools via prompt config — it just doesn't pass the user's original input, only the synthesized SUMMARIZATION_PROMPT. The model is expected to respond with text. The summary is extracted via `get_last_assistant_message_from_turn(history_items)` at `compact.rs:261`.

Compaction reuses the **same model and reasoning effort** as the current turn (`compact_remote.rs:191-202`). No model downgrade.

---

## 7. Events, persistence, resume

### Protocol events

Three event-level signals:

- `TurnItem::ContextCompaction(ContextCompactionItem)` — emitted as `ItemStartedEvent` at `compact.rs:177`, completed at `compact.rs:287`. This is what the TUI renders as a compaction row.
- `EventMsg::Warning` — posted immediately after success (`compact.rs:289-292`): *"Heads up: Long threads and multiple compactions can cause the model to be less accurate. Start a new thread when possible to keep threads small and targeted."*
- `CodexCompactionEvent` analytics (compact.rs:308-358) — `trigger` (Manual/Auto), `reason` (UserRequested/ContextLimit/ModelDownshift), `phase` (StandaloneTurn/PreTurn/MidTurn), `status` (Completed/Interrupted/Failed), `active_context_tokens_before`/`after`, duration. Not on the user event bus — analytics-only.

### Persistence

History replacement is atomic in `session/mod.rs:2609-2632`:

```rust
pub(crate) async fn replace_compacted_history(
    &self,
    items: Vec<ResponseItem>,
    reference_context_item: Option<TurnContextItem>,
    compacted_item: CompactedItem,
) {
    {
        let mut state = self.state.lock().await;
        state.replace_history(items, reference_context_item.clone());
        state.start_next_auto_compact_window();
    }
    this.persist_rollout_items(&[RolloutItem::Compacted(compacted_item)]).await;
    if let Some(turn_context_item) = reference_context_item {
        this.persist_rollout_items(&[RolloutItem::TurnContext(turn_context_item)]).await;
    }
}
```

`CompactedItem` (in `protocol/src/protocol.rs`) is intentionally simple:

```rust
pub struct CompactedItem {
    pub message: String,                              // summary text (local path)
    pub replacement_history: Option<Vec<ResponseItem>>,  // post-compaction history
}
```

### Resume

There is no special "replay from checkpoint" decision. The rollout file is the source of truth. On replay, sequential application of rollout items naturally has a `RolloutItem::Compacted` entry *overwrite* prior history when the reconstruction encounters it. See `tests/suite/compact_resume_fork.rs` for the resume-correctness tests.

This is elegant: **the compaction event is the checkpoint.** No second store, no checkpoint id.

---

## 8. TUI rendering

- Slash command registered: `tui/src/slash_command.rs:85`, popup gating in `bottom_pane/command_popup.rs:405`.
- The compaction history row is matched in `tui/src/chatwidget/replay.rs:168` and `tui/src/resume_picker.rs:210` against `ThreadItem::ContextCompaction { .. }`.
- The post-compaction warning is rendered through the normal `EventMsg::Warning` rail.
- Pre/post-compact hooks have first-class TUI rows in `tui/src/history_cell/hook_cell.rs:719-720`.

---

## 9. The most informative tests to read

Living in `core/tests/suite/`:

- `compact.rs::manual_compact_emits_context_compaction_items` — minimum proof of protocol events.
- `compact.rs::auto_compact_runs_after_token_limit_hit` — basic auto-trigger.
- `compact.rs::multiple_auto_compact_per_task_runs_after_token_limit_hit` — multi-compaction in one task.
- `compact.rs::auto_compact_persists_rollout_entries` — rollout persistence shape.
- `compact.rs::auto_compact_body_after_prefix_counts_growth_after_compaction` — proves the `BodyAfterPrefix` window logic.
- `compact.rs::snapshot_request_shape_mid_turn_continuation_compaction` — golden file of what the model sees post-compaction.
- `compact_resume_fork.rs` — resume correctness.
- `compact.rs::compact_hooks_respect_matchers_and_post_runs_after_compaction` — Pre/PostCompact hook lifecycle.

---

## 10. Implications for nav

Mapping the codex design onto the nav PRD:

1. **Adopt the compaction prompt verbatim.** It's a static template; no reason to reinvent. Keep both files (`prompt.md` and `summary_prefix.md`).
2. **Replicate the three-trigger model.** Manual, in-loop (post-sampling) auto, in-compaction overflow fallback. A "before-first-sampling" check on freshly submitted user prompts is *not* part of codex's design — it relies on the in-compaction fallback or a provider overflow error to recover. Don't add a pre-flight check just because PRD story #17 asks for it; re-examine that story first.
3. **Replicate the `InitialContextInjection` distinction.** This is subtle but load-bearing. Manual (and any hypothetical pre-flight) = `DoNotInject` and clear reference context, because the next regular turn will reinject canonical initial context. In-loop auto = `BeforeLastUserMessage`, because the model has been trained to see the summary as the last item in history.
4. **Carry forward `[recent user messages, capped at ~20k tokens] + [SUMMARY_PREFIX + summary]`.** Drop everything else. Filter prior summaries out of the "user messages" pool.
5. **Don't persist a separate compaction table.** Reuse the rollout/event log; let a `RolloutItem::Compacted` (or nav-equivalent) entry naturally shadow earlier items on replay. This satisfies PRD stories 8, 9, 10, 21, 24, 26, 27 in one stroke.
6. **Make compaction turns non-steerable**, with queued input draining post-compaction (PRD #14, #15).
7. **Threshold scope = `Total` for v1.** `BodyAfterPrefix` is sophisticated but only matters once you've done compaction more than once in a session and the summary itself is large. Ship `Total` first; revisit when telemetry shows pathological multi-compact sessions.
8. **Emit one analytics event with `trigger × reason × phase × status` axes** — codex's structure is the right shape.
9. **Keep nav's existing tool-call-pair overflow trim as the *in-compaction* retry**, not the primary recovery. Same role as `compact.rs:223-232`.
10. **Test priorities**: (a) ItemStarted/ItemCompleted protocol round-trip, (b) replay sends checkpoint + post-checkpoint only, (c) summary-message filter prevents re-summarization, (d) non-steerable behavior, (e) mid-turn vs pre-turn injection golden.

The cleanest single design choice in the codex implementation is treating compaction as **just another turn that happens to rewrite history**, persisted via the same rollout log. That's the property the PRD is gesturing at when it says "compaction should be a normal session lifecycle event, not an emergency truncation."
