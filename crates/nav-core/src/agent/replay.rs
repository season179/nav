use std::path::Path;

use serde_json::{Value, json};

use super::AgentEvent;
use super::compaction::{latest_checkpoint_slice, summary_message};
use super::runner::build_user_content;

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
/// `cwd` is the workspace root used to resolve image attachment paths back to
/// bytes. Images whose files are no longer readable are silently dropped, same
/// as the live agent loop — a missing attachment can't block resume.
///
/// Tool-call events stay in the persisted log for scrollback, but replay skips
/// them. Stateless Responses tool turns require the matching reasoning items,
/// and nav only keeps those encrypted reasoning blobs inside the active
/// `run_agent` loop, not in the long-term session log.
pub fn rebuild_responses_input(events: &[AgentEvent], cwd: &Path) -> Vec<Value> {
    if let Some(slice) = latest_checkpoint_slice(events) {
        let mut input = Vec::with_capacity(slice.following.len() + 1);
        input.push(summary_message(&slice.summary));
        for event in slice.following {
            push_replay_event(&mut input, event, cwd);
        }
        return input;
    }
    let mut input = Vec::new();
    for event in events {
        push_replay_event(&mut input, event, cwd);
    }
    input
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
        | AgentEvent::ToolCallStarted { .. }
        | AgentEvent::ToolCallOutput { .. }
        | AgentEvent::FileChange { .. }
        | AgentEvent::TurnDiff { .. }
        | AgentEvent::ToolCallApprovalRequest { .. }
        | AgentEvent::ToolCallBlocked { .. }
        | AgentEvent::TurnComplete { .. }
        | AgentEvent::ProviderRetry { .. }
        | AgentEvent::ContextTrimmed { .. }
        | AgentEvent::CompactionStarted { .. }
        | AgentEvent::CompactionCompleted { .. }
        | AgentEvent::CompactionFailed { .. }
        | AgentEvent::Error { .. } => {}
    }
}
