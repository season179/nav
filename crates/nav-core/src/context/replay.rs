use std::collections::HashSet;
use std::path::Path;

use serde_json::{Value, json};

use crate::agent_loop::AgentEvent;
use crate::agent_loop::runner::build_user_content;
use crate::context::compaction::{latest_checkpoint_slice, summary_message};

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
    if let Some(slice) = latest_checkpoint_slice(events) {
        let mut input = Vec::with_capacity(slice.following.len() + 1);
        input.push(summary_message(&slice.summary));
        push_replay_events(&mut input, slice.following, cwd);
        return input;
    }
    let mut input = Vec::new();
    push_replay_events(&mut input, events, cwd);
    input
}

fn push_replay_events(input: &mut Vec<Value>, events: &[AgentEvent], cwd: &Path) {
    let mut current_turn_start: Option<usize> = None;
    // `function_call` items become valid in the wire input only when the
    // matching reasoning/function_call continuation was captured. We track
    // `call_id`s seen in `ResponseContinuation` so a stray `ToolCallOutput`
    // from a session that predates continuation persistence doesn't surface
    // as an orphaned `function_call_output`.
    let mut pending_call_ids: HashSet<String> = HashSet::new();
    for event in events {
        match event {
            AgentEvent::UserMessage { .. } => {
                current_turn_start = Some(input.len());
                pending_call_ids.clear();
                push_replay_event(input, event, cwd);
            }
            AgentEvent::ResponseContinuation { items } => {
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
            AgentEvent::TurnComplete { .. }
            | AgentEvent::CompactionCompleted { .. }
            | AgentEvent::CompactionFailed { .. }
            | AgentEvent::Error { .. } => {
                push_replay_event(input, event, cwd);
                current_turn_start = None;
                pending_call_ids.clear();
            }
            AgentEvent::TurnAborted { .. } => {
                if let Some(start) = current_turn_start.take() {
                    input.truncate(start);
                }
                pending_call_ids.clear();
            }
            _ => push_replay_event(input, event, cwd),
        }
    }
}

fn push_replay_event(input: &mut Vec<Value>, event: &AgentEvent, cwd: &Path) {
    match event {
        AgentEvent::UserMessage {
            text, attachments, ..
        } => {
            input.push(json!({
                "type": "message",
                "role": "user",
                "content": build_user_content(text, attachments, cwd),
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
