use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Value, json};

use crate::agent_loop::AgentEvent;
use crate::agent_loop::runner::build_user_content;
use crate::context::compaction::{latest_checkpoint_slice, summary_message};
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
    if let Some(slice) = latest_checkpoint_slice(events) {
        let mut input = Vec::with_capacity(slice.following.len() + 1);
        input.push(summary_message(&slice.summary));
        push_replay_events(&mut input, slice.following, cwd);
        apply_replay_budget(&mut input, ReplayBudget::default());
        return input;
    }
    let mut input = Vec::new();
    push_replay_events(&mut input, events, cwd);
    apply_replay_budget(&mut input, ReplayBudget::default());
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

fn apply_replay_budget(input: &mut Vec<Value>, budget: ReplayBudget) {
    if input.is_empty() {
        return;
    }

    remove_orphan_outputs(input);
    let tool_names = tool_names_by_call_id(input);
    let protected = protected_call_ids(input, budget.raw_tool_turns);

    for idx in function_call_output_indices(input) {
        let Some(call_id) = call_id(&input[idx]).map(str::to_string) else {
            continue;
        };
        if protected.contains(&call_id) {
            continue;
        }
        let Some(tool_name) = tool_names.get(&call_id).map(String::as_str) else {
            continue;
        };
        if should_reduce_tool_output(tool_name, &input[idx]) {
            let output = output_text(&input[idx]).unwrap_or_default();
            let reduced = reduced_tool_output(tool_name, output, budget.max_raw_tool_output_bytes);
            set_output_text(&mut input[idx], reduced);
        }
    }

    clear_to_total_budget(input, &protected, budget.max_total_tool_output_bytes);
    remove_orphan_outputs(input);
}

fn remove_orphan_outputs(input: &mut Vec<Value>) {
    let call_ids: HashSet<String> = input
        .iter()
        .filter(|item| item_type(item) == Some("function_call"))
        .filter_map(|item| call_id(item).map(str::to_string))
        .collect();
    input.retain(|item| {
        item_type(item) != Some("function_call_output")
            || call_id(item).is_some_and(|id| call_ids.contains(id))
    });
}

fn tool_names_by_call_id(input: &[Value]) -> HashMap<String, String> {
    input
        .iter()
        .filter(|item| item_type(item) == Some("function_call"))
        .filter_map(|item| {
            let call_id = call_id(item)?;
            let name = item.get("name").and_then(Value::as_str)?;
            Some((call_id.to_string(), name.to_string()))
        })
        .collect()
}

fn protected_call_ids(input: &[Value], raw_tool_turns: usize) -> HashSet<String> {
    let mut protected = HashSet::new();
    if raw_tool_turns == 0 {
        return protected;
    }

    let user_indices: Vec<usize> = input
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item_type(item) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("user")
        })
        .map(|(idx, _)| idx)
        .collect();
    if user_indices.is_empty() {
        return protected;
    }

    let take = raw_tool_turns.min(user_indices.len());
    let protect_from = user_indices[user_indices.len() - take];
    for item in &input[protect_from..] {
        if item_type(item) == Some("function_call")
            && let Some(id) = call_id(item)
        {
            protected.insert(id.to_string());
        }
    }
    protected
}

fn function_call_output_indices(input: &[Value]) -> Vec<usize> {
    input
        .iter()
        .enumerate()
        .filter(|(_, item)| item_type(item) == Some("function_call_output"))
        .map(|(idx, _)| idx)
        .collect()
}

fn should_reduce_tool_output(tool_name: &str, item: &Value) -> bool {
    if is_error_output(item) || output_is_already_budgeted(item) {
        return false;
    }
    matches!(tool_name, "read_file" | "code_search" | "bash")
}

fn output_is_already_budgeted(item: &Value) -> bool {
    output_text(item).is_some_and(|text| {
        text.starts_with(CLEARED_TOOL_OUTPUT_PLACEHOLDER)
            || text.starts_with(REDUCED_TOOL_OUTPUT_PREFIX)
    })
}

fn is_error_output(item: &Value) -> bool {
    let text = output_text(item).unwrap_or_default();
    text.starts_with("tool error:") || (text.starts_with("tool ") && text.contains(" blocked:"))
}

fn reduced_tool_output(tool_name: &str, output: &str, max_bytes: usize) -> String {
    let header = if output.len() > max_bytes {
        format!(
            "{REDUCED_TOOL_OUTPUT_PREFIX}: {tool_name}; original {} bytes; showing first {max_bytes} bytes]\n",
            output.len(),
        )
    } else {
        format!(
            "{REDUCED_TOOL_OUTPUT_PREFIX}: {tool_name}; original {} bytes]\n",
            output.len()
        )
    };
    let preview_budget = max_bytes.saturating_sub(header.len());
    let preview = truncate_to_char_boundary(output, preview_budget);
    let mut reduced = String::with_capacity(header.len() + preview.len());
    reduced.push_str(&header);
    reduced.push_str(preview);
    reduced
}

fn clear_to_total_budget(input: &mut [Value], protected: &HashSet<String>, max_total_bytes: usize) {
    let mut total = total_tool_output_bytes(input);
    if total <= max_total_bytes {
        return;
    }

    for idx in function_call_output_indices(input) {
        if total <= max_total_bytes {
            break;
        }
        let Some(id) = call_id(&input[idx]) else {
            continue;
        };
        if protected.contains(id) {
            continue;
        }
        let old_len = output_text(&input[idx]).map(str::len).unwrap_or(0);
        set_output_text(&mut input[idx], CLEARED_TOOL_OUTPUT_PLACEHOLDER.to_string());
        total = total
            .saturating_sub(old_len)
            .saturating_add(CLEARED_TOOL_OUTPUT_PLACEHOLDER.len());
    }
}

fn total_tool_output_bytes(input: &[Value]) -> usize {
    input
        .iter()
        .filter(|item| item_type(item) == Some("function_call_output"))
        .filter_map(output_text)
        .map(str::len)
        .sum()
}

fn truncate_to_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn set_output_text(item: &mut Value, output: String) {
    if let Value::Object(fields) = item {
        fields.insert("output".to_string(), Value::String(output));
    }
}

fn output_text(item: &Value) -> Option<&str> {
    item.get("output").and_then(Value::as_str)
}

fn item_type(item: &Value) -> Option<&str> {
    item.get("type").and_then(Value::as_str)
}

fn call_id(item: &Value) -> Option<&str> {
    item.get("call_id").and_then(Value::as_str)
}
