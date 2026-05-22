//! Normalize Chat Completions SSE chunks into Responses-shape events.
//!
//! Chat Completions streams `chat.completion.chunk` frames; the rest of the
//! agent loop is wired against the Responses API event vocabulary
//! (`response.output_text.delta`, `response.output_item.done`,
//! `response.completed`). [`ChatCompletionsAccumulator`] holds the streaming
//! state — content buffer, tool calls keyed by `index`, captured usage — and
//! produces Responses-shape events as chunks arrive so
//! [`crate::model::responses::collector::ResponseCollector`] can fold both
//! backends through the same code path.

use anyhow::anyhow;
use serde_json::{Map, Value, json};

use crate::model::responses::{ResponsesError, is_overflow_code, is_overflow_message};

const MAX_STREAMING_TOOL_CALLS: usize = 128;

/// Streaming Chat Completions chunk accumulator.
///
/// Only `choices[0]` is consumed — parallel-choice streams are out of scope
/// for the issue; n>1 sampling is not part of the agent loop. Each
/// [`Self::push_chunk`] call folds one parsed chunk into the running state
/// and returns the events the chunk produces. [`Self::finalize`] runs at
/// stream end (after `[DONE]` or socket close) and emits any terminal event
/// the upstream chunk stream did not already flush via `finish_reason`.
#[derive(Default)]
pub(super) struct ChatCompletionsAccumulator {
    content_buffer: String,
    content_finalized: bool,
    tool_calls: Vec<ToolCallState>,
    tool_calls_finalized: bool,
    usage: Option<Value>,
    completed: bool,
}

#[derive(Default)]
struct ToolCallState {
    id: Option<String>,
    name: String,
    arguments: String,
}

impl ChatCompletionsAccumulator {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Fold one parsed `chat.completion.chunk` value into the running state
    /// and return any normalized events the chunk produces.
    ///
    /// - `delta.content` ⇒ `response.output_text.delta`.
    /// - `delta.tool_calls[*]` ⇒ appended to the per-index slot. No event is
    ///   emitted mid-stream; the fully assembled tool call ships in a single
    ///   `response.output_item.done` once `finish_reason` lands so the
    ///   downstream collector sees the same shape the Responses API would
    ///   have produced.
    /// - `finish_reason` ⇒ emits one `response.output_item.done` for the
    ///   accumulated message text (if non-empty) followed by one per
    ///   accumulated tool call in `index` order (0, 1, 2, …).
    /// - top-level `usage` ⇒ captured for the closing `response.completed`.
    /// - top-level `error` with `context_length_exceeded` /
    ///   `context_window_exceeded` ⇒ `ResponsesError::ContextWindowExceeded`
    ///   so the agent loop can run its one-shot compaction recovery. Any
    ///   other shape of top-level `error` becomes `ResponsesError::Other`.
    pub(super) fn push_chunk(&mut self, chunk: &Value) -> Result<Vec<Value>, ResponsesError> {
        if self.completed {
            return Ok(Vec::new());
        }

        if let Some(err) = chunk.get("error") {
            let (code, message) = chat_error_detail(err);
            if is_context_overflow_error(code, &message) {
                return Err(ResponsesError::ContextWindowExceeded { message });
            }
            return Err(ResponsesError::Other(anyhow!(
                "Chat Completions error: {message}"
            )));
        }

        // The trailing usage chunk in OpenAI-compatible streams carries an
        // empty `choices` array. Capture usage even when the chunk has no
        // choice to act on.
        if let Some(usage) = chunk.get("usage")
            && !usage.is_null()
        {
            self.usage = Some(normalize_usage(usage));
        }

        let mut events = Vec::new();

        // Only choice 0 is consumed. A missing `index` is treated as 0 so
        // providers that omit the field on single-choice streams (some
        // OpenAI-compatible servers) still work.
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| {
                choices.iter().find(|c| {
                    c.get("index")
                        .and_then(Value::as_u64)
                        .map(|i| i == 0)
                        .unwrap_or(true)
                })
            })
        else {
            return Ok(events);
        };

        if let Some(delta) = choice.get("delta") {
            if let Some(content) = delta.get("content").and_then(Value::as_str)
                && !content.is_empty()
            {
                self.content_buffer.push_str(content);
                events.push(json!({
                    "type": "response.output_text.delta",
                    "delta": content,
                }));
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc_delta in tool_calls {
                    self.apply_tool_call_delta(tc_delta)?;
                }
            }
        }

        if choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .is_some()
        {
            self.flush_finalized_items(&mut events);
            self.flush_completed(&mut events);
        }

        Ok(events)
    }

    /// Flush any items that `finish_reason` did not (abruptly closed streams,
    /// providers that skip the field) and emit the terminal
    /// `response.completed` event so the shared collector can wrap up.
    pub(super) fn finalize(mut self) -> Vec<Value> {
        let mut events = Vec::new();
        if self.completed {
            return events;
        }
        self.flush_finalized_items(&mut events);
        self.flush_completed(&mut events);
        events
    }

    fn flush_completed(&mut self, events: &mut Vec<Value>) {
        if self.completed {
            return;
        }
        self.completed = true;
        let mut response = Map::new();
        if let Some(usage) = self.usage.take() {
            response.insert("usage".into(), usage);
        }
        events.push(json!({
            "type": "response.completed",
            "response": Value::Object(response),
        }));
    }

    fn apply_tool_call_delta(&mut self, tc_delta: &Value) -> Result<(), ResponsesError> {
        let raw_index = tc_delta.get("index").and_then(Value::as_u64).unwrap_or(0);
        let index = usize::try_from(raw_index).map_err(|_| {
            ResponsesError::Other(anyhow!(
                "Chat Completions tool_call index {raw_index} cannot fit in usize"
            ))
        })?;
        if index >= MAX_STREAMING_TOOL_CALLS {
            return Err(ResponsesError::Other(anyhow!(
                "Chat Completions tool_call index {index} exceeds maximum supported count {MAX_STREAMING_TOOL_CALLS}"
            )));
        }
        while self.tool_calls.len() <= index {
            self.tool_calls.push(ToolCallState::default());
        }
        let slot = &mut self.tool_calls[index];
        if let Some(id) = tc_delta.get("id").and_then(Value::as_str)
            && !id.is_empty()
        {
            slot.id = Some(id.to_string());
        }
        if let Some(func) = tc_delta.get("function") {
            if let Some(name) = func.get("name").and_then(Value::as_str)
                && !name.is_empty()
            {
                slot.name = name.to_string();
            }
            if let Some(args) = func.get("arguments").and_then(Value::as_str) {
                slot.arguments.push_str(args);
            }
        }
        Ok(())
    }

    fn flush_finalized_items(&mut self, events: &mut Vec<Value>) {
        if !self.content_finalized && !self.content_buffer.is_empty() {
            self.content_finalized = true;
            let text = std::mem::take(&mut self.content_buffer);
            events.push(json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "content": [{
                        "type": "output_text",
                        "text": text,
                    }],
                },
            }));
        }
        if !self.tool_calls_finalized {
            self.tool_calls_finalized = true;
            for slot in self.tool_calls.drain(..) {
                events.push(tool_call_item(slot));
            }
        }
    }
}

fn tool_call_item(slot: ToolCallState) -> Value {
    json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "call_id": slot.id.unwrap_or_default(),
            "name": slot.name,
            "arguments": slot.arguments,
        },
    })
}

fn chat_error_detail(err: &Value) -> (Option<&str>, String) {
    let code = err.get("code").and_then(Value::as_str);
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| err.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| err.to_string());
    (code, message)
}

fn is_context_overflow_error(code: Option<&str>, message: &str) -> bool {
    is_overflow_code(code) || is_overflow_message(message)
}

/// Map Chat Completions `prompt_tokens` / `completion_tokens` (with their
/// `*_tokens_details` siblings) onto the Responses-shaped `input_tokens` /
/// `output_tokens` keys that
/// [`crate::model::responses::parser::turn_usage_from`] consumes. Unknown
/// fields are dropped; absent counts stay absent and default to 0 at the
/// parser.
fn normalize_usage(usage: &Value) -> Value {
    let prompt = usage.get("prompt_tokens").and_then(Value::as_u64);
    let completion = usage.get("completion_tokens").and_then(Value::as_u64);
    let cached = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64);
    let reasoning = usage
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_u64);
    let mut out = Map::new();
    if let Some(p) = prompt {
        out.insert("input_tokens".into(), json!(p));
    }
    if let Some(c) = completion {
        out.insert("output_tokens".into(), json!(c));
    }
    if let Some(c) = cached {
        out.insert("input_tokens_details".into(), json!({"cached_tokens": c}));
    }
    if let Some(r) = reasoning {
        out.insert(
            "output_tokens_details".into(),
            json!({"reasoning_tokens": r}),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_chunks(chunks: &[Value]) -> (Vec<Value>, Option<ResponsesError>) {
        let mut acc = ChatCompletionsAccumulator::new();
        let mut events = Vec::new();
        for chunk in chunks {
            match acc.push_chunk(chunk) {
                Ok(evs) => events.extend(evs),
                Err(err) => return (events, Some(err)),
            }
        }
        events.extend(acc.finalize());
        (events, None)
    }

    fn type_of(event: &Value) -> &str {
        event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    #[test]
    fn pure_text_stream_emits_deltas_then_message_done_then_completed() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
            json!({"choices":[{"index":0,"delta":{"content":"Hel"}}]}),
            json!({"choices":[{"index":0,"delta":{"content":"lo"}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2}}),
        ];
        let (events, err) = run_chunks(&chunks);
        assert!(err.is_none());

        let kinds: Vec<&str> = events.iter().map(type_of).collect();
        assert_eq!(
            kinds,
            vec![
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_item.done",
                "response.completed",
            ]
        );
        assert_eq!(events[0]["delta"], "Hel");
        assert_eq!(events[1]["delta"], "lo");
        assert_eq!(events[2]["item"]["type"], "message");
        assert_eq!(events[2]["item"]["content"][0]["text"], "Hello");
        assert_eq!(events[3]["response"]["usage"]["input_tokens"], 5);
        assert_eq!(events[3]["response"]["usage"]["output_tokens"], 2);
    }

    #[test]
    fn empty_content_chunks_do_not_emit_deltas() {
        // The opening role:"assistant",content:"" chunk and the trailing
        // finish_reason chunk with delta:{} must not synthesize empty text
        // deltas or empty message items.
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ];
        let (events, err) = run_chunks(&chunks);
        assert!(err.is_none());
        // Only response.completed should be emitted: no content, no tool calls.
        let kinds: Vec<&str> = events.iter().map(type_of).collect();
        assert_eq!(kinds, vec!["response.completed"]);
    }

    #[test]
    fn single_tool_call_assembles_arguments_into_one_output_item() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":null}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"main.rs\"}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let (events, err) = run_chunks(&chunks);
        assert!(err.is_none());
        // No text => only tool-call item + completed.
        let kinds: Vec<&str> = events.iter().map(type_of).collect();
        assert_eq!(
            kinds,
            vec!["response.output_item.done", "response.completed"]
        );
        let item = &events[0]["item"];
        assert_eq!(item["type"], "function_call");
        assert_eq!(item["call_id"], "call_1");
        assert_eq!(item["name"], "read_file");
        assert_eq!(item["arguments"], "{\"path\":\"main.rs\"}");
        // The accumulated arguments string must be valid JSON the local tool
        // can parse — the acceptance criterion calls this out explicitly.
        let parsed: Value = serde_json::from_str(item["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(parsed["path"], "main.rs");
    }

    #[test]
    fn parallel_tool_calls_materialize_in_index_order() {
        // Index 1's first delta arrives while index 0 is still mid-arguments.
        // The accumulator must keep them apart and emit (0, 1) at finish.
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":""}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"bash","arguments":""}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"command\":\"ls\"}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let (events, err) = run_chunks(&chunks);
        assert!(err.is_none());
        let items: Vec<&Value> = events
            .iter()
            .filter(|e| type_of(e) == "response.output_item.done")
            .collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["item"]["call_id"], "call_a");
        assert_eq!(items[0]["item"]["name"], "read_file");
        assert_eq!(items[0]["item"]["arguments"], "{\"path\":\"a.rs\"}");
        assert_eq!(items[1]["item"]["call_id"], "call_b");
        assert_eq!(items[1]["item"]["name"], "bash");
        assert_eq!(items[1]["item"]["arguments"], "{\"command\":\"ls\"}");
    }

    #[test]
    fn text_then_tool_call_emits_message_before_function_call() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"content":"Hi "}}]}),
            json!({"choices":[{"index":0,"delta":{"content":"there"}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"bash","arguments":"{}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ];
        let (events, err) = run_chunks(&chunks);
        assert!(err.is_none());
        let items: Vec<&Value> = events
            .iter()
            .filter(|e| type_of(e) == "response.output_item.done")
            .collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["item"]["type"], "message");
        assert_eq!(items[0]["item"]["content"][0]["text"], "Hi there");
        assert_eq!(items[1]["item"]["type"], "function_call");
        assert_eq!(items[1]["item"]["call_id"], "c1");
    }

    #[test]
    fn context_overflow_error_chunk_surfaces_as_context_window_exceeded() {
        let chunk = json!({
            "error": {
                "code": "context_length_exceeded",
                "message": "Your input is too long.",
            }
        });
        let mut acc = ChatCompletionsAccumulator::new();
        let err = acc.push_chunk(&chunk).unwrap_err();
        match err {
            ResponsesError::ContextWindowExceeded { message } => {
                assert!(message.contains("too long"));
            }
            other => panic!("expected ContextWindowExceeded, got {other}"),
        }
    }

    #[test]
    fn other_error_chunk_surfaces_as_other_error() {
        let chunk = json!({
            "error": {
                "code": "invalid_request_error",
                "message": "Bad request.",
            }
        });
        let mut acc = ChatCompletionsAccumulator::new();
        let err = acc.push_chunk(&chunk).unwrap_err();
        match err {
            ResponsesError::Other(e) => assert!(e.to_string().contains("Bad request")),
            other => panic!("expected Other, got {other}"),
        }
    }

    #[test]
    fn bare_string_error_preserves_detail_and_detects_context_overflow() {
        let mut acc = ChatCompletionsAccumulator::new();
        let err = acc
            .push_chunk(&json!({"error": "context length 8192 exceeded"}))
            .unwrap_err();
        match err {
            ResponsesError::ContextWindowExceeded { message } => {
                assert!(message.contains("8192"));
            }
            other => panic!("expected ContextWindowExceeded, got {other}"),
        }

        let mut acc = ChatCompletionsAccumulator::new();
        let err = acc
            .push_chunk(&json!({"error": "provider heartbeat failed"}))
            .unwrap_err();
        match err {
            ResponsesError::Other(e) => {
                assert!(e.to_string().contains("provider heartbeat failed"))
            }
            other => panic!("expected Other, got {other}"),
        }
    }

    #[test]
    fn oversized_tool_call_index_is_rejected_without_growing_state() {
        let mut acc = ChatCompletionsAccumulator::new();
        let err = acc
            .push_chunk(&json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 4_000_000_000u64,
                            "function": {"arguments": ""}
                        }]
                    }
                }]
            }))
            .unwrap_err();
        match err {
            ResponsesError::Other(e) => {
                assert!(e.to_string().contains("tool_call index"));
            }
            other => panic!("expected Other, got {other}"),
        }
        assert!(acc.tool_calls.is_empty());
    }

    #[test]
    fn finalize_without_finish_reason_still_flushes_accumulated_state() {
        // A socket close mid-tool-call (no finish_reason ever lands) must
        // still surface what the model managed to emit so the agent loop
        // sees a coherent envelope. The fall-through path is the only
        // reason `finalize` re-runs `flush_finalized_items`.
        let mut acc = ChatCompletionsAccumulator::new();
        let _ = acc
            .push_chunk(&json!({"choices":[{"index":0,"delta":{"content":"partial"}}]}))
            .unwrap();
        let _ = acc
            .push_chunk(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"noop","arguments":"{}"}}]}}]}))
            .unwrap();
        let events = acc.finalize();
        let kinds: Vec<&str> = events.iter().map(type_of).collect();
        assert_eq!(
            kinds,
            vec![
                "response.output_item.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        assert_eq!(events[0]["item"]["type"], "message");
        assert_eq!(events[1]["item"]["type"], "function_call");
    }

    #[test]
    fn usage_maps_to_responses_shape_with_details() {
        let chunks = vec![
            json!({"choices":[{"index":0,"delta":{"content":"hi"}}]}),
            json!({
                "choices": [],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 50,
                    "prompt_tokens_details": {"cached_tokens": 20},
                    "completion_tokens_details": {"reasoning_tokens": 10},
                }
            }),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ];
        let (events, err) = run_chunks(&chunks);
        assert!(err.is_none());
        let completed = events
            .iter()
            .find(|e| type_of(e) == "response.completed")
            .unwrap();
        let usage = &completed["response"]["usage"];
        assert_eq!(usage["input_tokens"], 100);
        assert_eq!(usage["output_tokens"], 50);
        assert_eq!(usage["input_tokens_details"]["cached_tokens"], 20);
        assert_eq!(usage["output_tokens_details"]["reasoning_tokens"], 10);
    }

    #[test]
    fn choices_without_explicit_index_default_to_zero() {
        // Some OpenAI-compatible servers omit `index` on single-choice
        // streams. Treat them as choice 0 instead of dropping the chunk.
        let chunks = vec![
            json!({"choices":[{"delta":{"content":"Hi"}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        ];
        let (events, err) = run_chunks(&chunks);
        assert!(err.is_none());
        let kinds: Vec<&str> = events.iter().map(type_of).collect();
        assert_eq!(
            kinds,
            vec![
                "response.output_text.delta",
                "response.output_item.done",
                "response.completed",
            ]
        );
    }
}
