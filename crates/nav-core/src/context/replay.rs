use std::collections::HashSet;
use std::path::Path;

use serde_json::{Value, json};

use crate::agent_loop::AgentEvent;
use crate::context::build_user_content;
use crate::context::compaction::{latest_checkpoint_slice, summary_message};
use crate::context::history::{ModelCapabilities, normalize_for_request};
use crate::context::replay_policy::ReplayBudget;

/// Exact `function_call_output` text that marks a cleared old tool output.
/// The string is part of the wire format so inspectors can classify the item
/// without an out-of-band signal.
pub const CLEARED_TOOL_OUTPUT_PLACEHOLDER: &str =
    "[Old tool result content cleared; original output is available in session log]";

/// Prefix of a `function_call_output` whose body has been reduced to a compact
/// summary instead of fully cleared. Same wire-format contract as above.
pub const REDUCED_TOOL_OUTPUT_PREFIX: &str = "[Reduced tool output";

/// Reconstructs the Responses API `input` array from a previously persisted
/// event log so that `--resume` can replay the same conversation state.
///
/// Translates durable user/assistant messages back into the wire-format item the
/// `Responses` create endpoint expects:
/// - `UserMessage` -> `{type: message, role: user, content: text}` (or a
///   typed-parts array when image attachments are present)
/// - `AssistantMessageDone` -> `{type: message, role: assistant, content: text}`
///
/// If the event log contains a [`AgentEvent::CompactionCompleted`] checkpoint,
/// replay is sliced from that checkpoint: the persisted summary is replayed as
/// a synthesized user message, followed only by the events recorded *after*
/// the checkpoint, so a resumed compacted session never silently expands back
/// to the full pre-compaction transcript.
///
/// `cwd` is the workspace root used to resolve image and file attachment
/// paths back to bytes. Attachments whose files are no longer readable are
/// silently dropped, same as the live agent loop — a missing attachment
/// can't block resume.
///
/// Tool-call replay is unlocked by `ResponseContinuation` events: each one
/// carries the sanitized `function_call` (and encrypted reasoning) items the
/// model emitted in a single response, so the next turn can resend the same
/// wire shape `store: false` needs. Matching `ToolCallOutput` events are then
/// translated back into `function_call_output` items. Older sessions without
/// `ResponseContinuation` events drop tool I/O during replay (their reasoning
/// continuation was never persisted, and a `function_call_output` without its
/// `function_call` would be rejected on submit).
pub fn rebuild_responses_input(events: &[AgentEvent], cwd: &Path) -> Vec<Value> {
    let mut input = if let Some(slice) = latest_checkpoint_slice(events) {
        let mut input = Vec::with_capacity(slice.following.len() + 1);
        input.push(summary_message(&slice.summary));
        push_replay_events(&mut input, slice.following, cwd);
        input
    } else {
        let mut input = Vec::new();
        push_replay_events(&mut input, events, cwd);
        input
    };
    // Replay normalizes for a permissive model: image stripping needs the
    // resolved model name, which the live runner has but `rebuild_*` does
    // not. The runner re-runs `strip_unsupported_images` on every turn so
    // the next iteration after a resume reaches the same wire shape.
    normalize_for_request(
        &mut input,
        &ModelCapabilities::permissive(),
        &ReplayBudget::default(),
    );
    input
}

fn push_replay_events(input: &mut Vec<Value>, events: &[AgentEvent], cwd: &Path) {
    // `user_turn_start` anchors replay back to the most recent UserMessage
    // and is *not* cleared by per-iteration `TurnComplete` events — a
    // single user prompt can drive multiple tool-call iterations, each
    // emitting its own `TurnComplete`, and an abort that lands after a
    // mid-prompt iteration still needs to drop every continuation and
    // tool output emitted since the user's last message. Without this,
    // resume would replay a partial tool-call state the user explicitly
    // aborted.
    //
    // The companion `last_iter_was_terminal` flag distinguishes "mid-turn
    // abort" (truncate back to UserMessage) from "abort fired *before* a
    // new turn even emitted its UserMessage" (don't touch the previous
    // turn). Terminal iters never emit a `ResponseContinuation` (only iters
    // that produced function_calls do), so a `TurnComplete` that follows
    // *no* `ResponseContinuation` since the last `UserMessage`/`TurnComplete`
    // marks the user turn as fully complete.
    let mut user_turn_start: Option<usize> = None;
    let mut iter_had_continuation = false;
    let mut last_iter_was_terminal = false;
    // `function_call` items become valid in the wire input only when the
    // matching reasoning/function_call continuation was captured. We track
    // `call_id`s seen in `ResponseContinuation` so a stray `ToolCallOutput`
    // from a session that predates continuation persistence doesn't surface
    // as an orphaned `function_call_output`.
    let mut pending_call_ids: HashSet<String> = HashSet::new();
    for event in events {
        match event {
            AgentEvent::UserMessage { .. } => {
                user_turn_start = Some(input.len());
                pending_call_ids.clear();
                iter_had_continuation = false;
                last_iter_was_terminal = false;
                push_replay_event(input, event, cwd);
            }
            AgentEvent::ResponseContinuation { items } => {
                iter_had_continuation = true;
                for item in items {
                    if item.get("type").and_then(Value::as_str) == Some("function_call")
                        && let Some(call_id) = item.get("call_id").and_then(Value::as_str)
                    {
                        pending_call_ids.insert(call_id.to_string());
                    }
                    input.push(item.clone());
                }
            }
            AgentEvent::ToolCallOutput {
                call_id, output, ..
            } => {
                if pending_call_ids.remove(call_id) {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                }
            }
            AgentEvent::CompactionCompleted { .. }
            | AgentEvent::CompactionFailed { .. }
            | AgentEvent::Error { .. } => {
                push_replay_event(input, event, cwd);
                user_turn_start = None;
                pending_call_ids.clear();
                iter_had_continuation = false;
                last_iter_was_terminal = false;
            }
            AgentEvent::TurnComplete { .. } => {
                // `TurnComplete` fires *per provider iteration*, not per
                // user turn — clearing the anchor here would let a later
                // abort within the same user prompt leave mid-prompt
                // continuation items in the replayed input. Keep the
                // anchor; the next `UserMessage` (or terminal event
                // above) is the right place to drop it.
                //
                // We do snapshot terminal-ness here: a `TurnComplete` that
                // ran with no `ResponseContinuation` this iter came from
                // the `calls.is_empty()` branch in the runner — the user
                // turn is fully done. The companion `TurnAborted` handler
                // uses this to ignore aborts that landed *after* a turn
                // already completed (e.g. attachment-guard rejection for
                // the *next* turn before it could emit its own UserMessage).
                last_iter_was_terminal = !iter_had_continuation;
                iter_had_continuation = false;
                push_replay_event(input, event, cwd);
            }
            AgentEvent::TurnAborted { .. } => {
                if last_iter_was_terminal {
                    // Abort fired after a fully-completed turn — the prior
                    // UserMessage is sealed. Leave it (and its tool I/O)
                    // intact; only disarm so a subsequent abort within a
                    // fresh active turn truncates correctly.
                    user_turn_start = None;
                } else if let Some(start) = user_turn_start.take() {
                    input.truncate(start);
                }
                pending_call_ids.clear();
                iter_had_continuation = false;
                last_iter_was_terminal = false;
            }
            _ => push_replay_event(input, event, cwd),
        }
    }
}

fn push_replay_event(input: &mut Vec<Value>, event: &AgentEvent, cwd: &Path) {
    match event {
        AgentEvent::UserMessage {
            text,
            display_text,
            attachments,
        } => {
            input.push(json!({
                "type": "message",
                "role": "user",
                "content": build_user_content(text, display_text.as_deref(), attachments, cwd),
            }));
        }
        AgentEvent::AssistantMessageDone { text } => {
            input.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}],
            }));
        }
        AgentEvent::AssistantMessageDelta { .. }
        | AgentEvent::ResponseContinuation { .. }
        | AgentEvent::ToolCallStarted { .. }
        | AgentEvent::ToolCallOutput { .. }
        | AgentEvent::SubagentStarted { .. }
        | AgentEvent::SubagentCompleted { .. }
        | AgentEvent::SubagentFailed { .. }
        | AgentEvent::FileChange { .. }
        | AgentEvent::TurnDiff { .. }
        | AgentEvent::GitCheckpoint { .. }
        | AgentEvent::SessionRewound { .. }
        | AgentEvent::ToolCallApprovalRequest { .. }
        | AgentEvent::ToolCallApprovalDecision { .. }
        | AgentEvent::ToolCallBlocked { .. }
        | AgentEvent::PendingInputQueued { .. }
        | AgentEvent::PendingInputEdited { .. }
        | AgentEvent::PendingInputRemoved { .. }
        | AgentEvent::PendingInputCleared { .. }
        | AgentEvent::PendingInputDequeued { .. }
        | AgentEvent::TurnComplete { .. }
        | AgentEvent::TurnAborted { .. }
        | AgentEvent::ProviderRetry { .. }
        | AgentEvent::ContextTrimmed { .. }
        | AgentEvent::ToolBudgetWarning { .. }
        | AgentEvent::CompactionStarted { .. }
        | AgentEvent::CompactionCompleted { .. }
        | AgentEvent::CompactionFailed { .. }
        | AgentEvent::Error { .. } => {}
    }
}
