use std::path::Path;

use serde_json::{Value, json};

use super::AgentEvent;
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
/// `cwd` is the workspace root used to resolve image attachment paths back to
/// bytes. Images whose files are no longer readable are silently dropped, same
/// as the live agent loop — a missing attachment can't block resume.
///
/// Tool-call events stay in the persisted log for scrollback, but replay skips
/// them. Stateless Responses tool turns require the matching reasoning items,
/// and nav only keeps those encrypted reasoning blobs inside the active
/// `run_agent` loop, not in the long-term session log.
pub fn rebuild_responses_input(events: &[AgentEvent], cwd: &Path) -> Vec<Value> {
    let mut input = Vec::new();
    for event in events {
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
            | AgentEvent::TurnComplete { .. }
            | AgentEvent::ProviderRetry { .. }
            | AgentEvent::ContextTrimmed { .. }
            | AgentEvent::Error { .. } => {}
        }
    }
    input
}
