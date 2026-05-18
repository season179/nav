use serde_json::{Value, json};

use super::AgentEvent;

/// Reconstructs the Responses API `input` array from a previously persisted
/// event log so that `--resume` can replay the same conversation state.
///
/// Translates durable user/assistant messages back into the wire-format item the
/// `Responses` create endpoint expects:
/// - `UserMessage` -> `{type: message, role: user, content: text}`
/// - `AssistantMessageDone` -> `{type: message, role: assistant, content: text}`
///
/// Tool-call events stay in the persisted log for scrollback, but replay skips
/// them. Stateless Responses tool turns require the matching reasoning items,
/// and nav only keeps those encrypted reasoning blobs inside the active
/// `run_agent` loop, not in the long-term session log.
pub fn rebuild_responses_input(events: &[AgentEvent]) -> Vec<Value> {
    let mut input = Vec::new();
    for event in events {
        match event {
            AgentEvent::UserMessage { text, .. } => {
                input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": text,
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
            | AgentEvent::TurnComplete { .. }
            | AgentEvent::Error { .. } => {}
        }
    }
    input
}
