//! Chat Completions response parsing.
//!
//! Mirrors [`crate::model::responses::parser`] so the agent loop can call
//! either backend without branching on transport.
//!
//! [`super::delta::ChatCompletionsAccumulator`] normalizes CC SSE chunks into
//! Responses-shaped events, so the collected [`ResponseEnvelope`] is
//! structurally identical to what the Responses backend produces. Most helpers
//! here delegate to the Responses parser directly. CC-specific differences:
//!
//! - [`sanitize_continuation_items`]: drops assistant message items just like
//!   the Responses path; `AssistantMessageDone` is already the durable text
//!   record in the session log.
//! - [`turn_usage_from`]: `tokens_reasoning` is always 0.

use crate::agent_loop::TurnUsage;
use crate::model::responses;
use crate::model::responses::ToolCall;
use crate::model::responses::types::ResponseEnvelope;
use anyhow::Result;
use serde_json::Value;

/// Extract tool calls from a collected Chat Completions envelope.
///
/// Delegates to [`responses::function_calls_from`] (not
/// [`responses::process_response`], which includes a stdout side-effect
/// the agent loop avoids by using the no-print variant directly).
pub(crate) fn process_response(response: &ResponseEnvelope) -> Result<Vec<ToolCall>> {
    responses::function_calls_from(response)
}

/// Concatenated text of every assistant message in the envelope.
/// Delegates to [`responses::assistant_text`].
pub(crate) fn assistant_text(response: &ResponseEnvelope) -> Option<String> {
    responses::assistant_text(response)
}

/// Extract raw output items for turn continuation.
/// Delegates to [`responses::into_raw_output`].
pub(crate) fn into_raw_output(response: ResponseEnvelope) -> Vec<Value> {
    responses::into_raw_output(response)
}

/// Strip continuation items down to the replay-only shapes nav persists.
///
/// Chat Completions has no encrypted reasoning items, but message items still
/// need the same treatment as Responses: live text is already durable as
/// `AssistantMessageDone`, so persisting the message again in
/// `ResponseContinuation` would replay duplicate assistant text.
pub(crate) fn sanitize_continuation_items(items: &[Value]) -> Vec<Value> {
    responses::sanitize_continuation_items(items)
}

/// Map usage fields into [`TurnUsage`].
///
/// The accumulator normalizes `prompt_tokens` → `input_tokens` etc. before
/// they reach the envelope, so this delegates to [`responses::turn_usage_from`]
/// and zeroes `tokens_reasoning` (CC has no standard reasoning token field).
/// Missing `usage` block returns [`TurnUsage::default()`].
pub(crate) fn turn_usage_from(response: &ResponseEnvelope) -> TurnUsage {
    let mut usage = responses::turn_usage_from(response);
    // CC has no standard reasoning token field; any value from the shared
    // parser is a normalization artifact, so zero it explicitly.
    usage.tokens_reasoning = 0;
    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::chat_completions::delta::ChatCompletionsAccumulator;
    use crate::model::responses::ResponseCollector;
    use serde_json::json;

    /// Drive SSE chunks through accumulator → collector → envelope, then
    /// return the envelope for assertion.
    fn collect_envelope(chunks: &[Value]) -> ResponseEnvelope {
        let mut acc = ChatCompletionsAccumulator::new();
        let mut collector = ResponseCollector::default();
        for chunk in chunks {
            for event in acc.push_chunk(chunk).unwrap() {
                collector.push_event(&event, "test").unwrap();
            }
        }
        for event in acc.finalize() {
            collector.push_event(&event, "test").unwrap();
        }
        collector.finish("test").unwrap()
    }

    #[test]
    fn process_response_extracts_tool_calls() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":""}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"main.rs\"}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let envelope = collect_envelope(&chunks);
        let calls = process_response(&envelope).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "call_1");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "main.rs");
    }

    #[test]
    fn process_response_returns_empty_for_text_only() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"content":"Hello"}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ];
        let envelope = collect_envelope(&chunks);
        let calls = process_response(&envelope).unwrap();
        assert!(calls.is_empty());
    }

    #[test]
    fn assistant_text_extracts_content() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"content":"Hello! "}}]}),
            json!({"choices":[{"index":0,"delta":{"content":"World."}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ];
        let envelope = collect_envelope(&chunks);
        assert_eq!(assistant_text(&envelope).as_deref(), Some("Hello! World."));
    }

    #[test]
    fn assistant_text_returns_none_for_tool_only() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let envelope = collect_envelope(&chunks);
        assert!(assistant_text(&envelope).is_none());
    }

    #[test]
    fn sanitize_continuation_items_drops_message_items() {
        let items = vec![
            json!({"type": "message", "content": [{"type": "output_text", "text": "hi"}]}),
            json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{}"}),
        ];
        let result = sanitize_continuation_items(&items);
        assert_eq!(
            result,
            vec![json!({
                "type": "function_call",
                "call_id": "c1",
                "name": "bash",
                "arguments": "{}",
            })]
        );
    }

    #[test]
    fn sanitize_continuation_items_empty_is_identity() {
        let result = sanitize_continuation_items(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn into_raw_output_returns_collected_items() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"content":"x"}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ];
        let envelope = collect_envelope(&chunks);
        let raw = into_raw_output(envelope);
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0]["type"], "message");
        assert_eq!(raw[0]["content"][0]["text"], "x");
    }

    #[test]
    fn turn_usage_from_maps_tokens_and_zeros_reasoning() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"content":"hi"}}]}),
            json!({
                "choices": [],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 50,
                    "prompt_tokens_details": {"cached_tokens": 20},
                }
            }),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ];
        let envelope = collect_envelope(&chunks);
        let usage = turn_usage_from(&envelope);
        assert_eq!(usage.tokens_input, 100);
        assert_eq!(usage.tokens_output, 50);
        assert_eq!(usage.tokens_input_cached, 20);
        assert_eq!(usage.tokens_reasoning, 0);
    }

    #[test]
    fn turn_usage_from_defaults_to_zero_when_no_usage() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"content":"hi"}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ];
        let envelope = collect_envelope(&chunks);
        let usage = turn_usage_from(&envelope);
        assert_eq!(usage, TurnUsage::default());
    }
}
