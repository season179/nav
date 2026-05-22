//! Persisted Responses-shape history → Chat Completions `messages` array.
//!
//! Pure converter run once per turn by [`super::request::build_request_body`]
//! (C1). nav stores turn continuation as Responses-shaped items so the OpenAI
//! Responses backend can echo them back unchanged; when the resolved provider
//! speaks Chat Completions, we translate just before serialization.
//!
//! Per-shape mapping (issue #106):
//!
//! - `message` / `role: user` → `{role: "user", content: "..."}`. `content`
//!   may already be a string or an array of parts; only `input_text` parts
//!   are carried forward (image parts are out of scope — handled by the file
//!   follow-up issue).
//! - `message` / `role: assistant` → `{role: "assistant", content: "..."}`.
//!   Consecutive assistant items merge into one message (Chat Completions
//!   rejects adjacent assistant turns). `output_text` and `text` parts get
//!   concatenated.
//! - `function_call` collapses into the assistant message currently being
//!   assembled. With no preceding assistant text, emits a standalone
//!   `{role: "assistant", content: null, tool_calls: [...]}`. Parallel
//!   function_calls share one `tool_calls` array.
//! - `function_call_output` → `{role: "tool", tool_call_id, content}`. This
//!   closes any in-flight assistant message.
//! - `reasoning` items are dropped. Chat Completions has no equivalent.
//!
//! The system instructions message is NOT prepended here — the request
//! builder owns insertion at index 0 so the same instructions composition
//! logic serves both backends.
//!
//! ## Preconditions
//!
//! The converter assumes its input has already been paired:
//! [`crate::context::history::normalize_for_request`] (or equivalent
//! orphan-repair) must run on the persisted items first. An orphan
//! `function_call_output` translates to a `role: "tool"` message with no
//! preceding assistant `tool_calls`, which Chat Completions providers
//! reject. Items missing a `call_id` are dropped — keeping them would
//! produce wire messages the API would 400 on with an opaque error.

use serde_json::{Map, Value, json};

/// Convert nav's persisted Responses-shaped history into the `messages` array
/// expected by `POST {base_url}/chat/completions`. See module-level docs for
/// the per-shape rules and merging behavior.
#[allow(dead_code)]
pub(crate) fn responses_items_to_chat_messages(items: &[Value]) -> Vec<Value> {
    let mut messages = Vec::with_capacity(items.len());
    let mut pending: Option<PendingAssistant> = None;

    for item in items {
        let Some(kind) = item.get("type").and_then(Value::as_str) else {
            continue;
        };
        match kind {
            "reasoning" => continue,
            // Missing `role` defaults to assistant: nav's Responses parser
            // does not deserialize the role field (see `types.rs`
            // `ResponseItem::Message`), so role-less message items already
            // exist in the codebase as a convention for assistant outputs.
            "message" => match item
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("assistant")
            {
                "user" => {
                    flush(&mut pending, &mut messages);
                    messages.push(json!({
                        "role": "user",
                        "content": extract_user_text(item),
                    }));
                }
                "assistant" => {
                    let text = extract_assistant_text(item);
                    if !text.is_empty() {
                        pending
                            .get_or_insert_with(PendingAssistant::default)
                            .text
                            .push_str(&text);
                    }
                }
                _ => continue,
            },
            "function_call" => {
                let Some(tool_call) = tool_call_value(item) else {
                    continue;
                };
                pending
                    .get_or_insert_with(PendingAssistant::default)
                    .tool_calls
                    .push(tool_call);
            }
            "function_call_output" => {
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    continue;
                };
                flush(&mut pending, &mut messages);
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": item.get("output").and_then(Value::as_str).unwrap_or(""),
                }));
            }
            _ => continue,
        }
    }

    flush(&mut pending, &mut messages);
    messages
}

#[derive(Default)]
struct PendingAssistant {
    text: String,
    tool_calls: Vec<Value>,
}

fn flush(pending: &mut Option<PendingAssistant>, out: &mut Vec<Value>) {
    let Some(p) = pending.take() else { return };
    let has_text = !p.text.is_empty();
    let has_calls = !p.tool_calls.is_empty();
    if !has_text && !has_calls {
        return;
    }
    let mut msg = Map::new();
    msg.insert("role".into(), Value::String("assistant".into()));
    // Empty assistant text normalizes to null, never "" — Chat Completions
    // treats "" as a positive empty turn, which confuses some providers.
    msg.insert(
        "content".into(),
        if has_text {
            Value::String(p.text)
        } else {
            Value::Null
        },
    );
    if has_calls {
        msg.insert("tool_calls".into(), Value::Array(p.tool_calls));
    }
    out.push(Value::Object(msg));
}

fn extract_user_text(item: &Value) -> String {
    match item.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut buf = String::new();
            let mut has_non_text = false;
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("input_text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            buf.push_str(text);
                        }
                    }
                    Some(_) => has_non_text = true,
                    None => {}
                }
            }
            // Multimodal parts (input_image, etc.) are out of scope for this
            // converter — the file follow-up will replace this placeholder
            // with proper Chat Completions content parts. Until then, emit
            // an honest marker so an image-only user turn doesn't silently
            // arrive at the model as an empty string.
            if buf.is_empty() && has_non_text {
                return "[non-text attachment omitted]".to_string();
            }
            buf
        }
        _ => String::new(),
    }
}

fn extract_assistant_text(item: &Value) -> String {
    match item.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut buf = String::new();
            for part in parts {
                let kind = part.get("type").and_then(Value::as_str);
                if (kind == Some("output_text") || kind == Some("text"))
                    && let Some(text) = part.get("text").and_then(Value::as_str)
                {
                    buf.push_str(text);
                }
            }
            buf
        }
        _ => String::new(),
    }
}

fn tool_call_value(item: &Value) -> Option<Value> {
    let call_id = item.get("call_id").and_then(Value::as_str)?;
    Some(json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
            "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or(""),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(items: &[Value]) -> String {
        serde_json::to_string_pretty(&responses_items_to_chat_messages(items)).unwrap()
    }

    #[test]
    fn pure_user_assistant_exchange() {
        let items = vec![
            json!({"type": "message", "role": "user", "content": "hello"}),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi there"}],
            }),
            json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "thanks"}],
            }),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "hello",
            "role": "user"
          },
          {
            "content": "hi there",
            "role": "assistant"
          },
          {
            "content": "thanks",
            "role": "user"
          }
        ]
        "#);
    }

    #[test]
    fn exchange_with_tool_call_collapses_into_assistant() {
        let items = vec![
            json!({"type": "message", "role": "user", "content": "list files"}),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Sure, running ls."}],
            }),
            json!({
                "type": "function_call",
                "call_id": "c1",
                "name": "bash",
                "arguments": "{\"cmd\":\"ls\"}",
            }),
            json!({"type": "function_call_output", "call_id": "c1", "output": "a.txt\nb.txt"}),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Two files."}],
            }),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "list files",
            "role": "user"
          },
          {
            "content": "Sure, running ls.",
            "role": "assistant",
            "tool_calls": [
              {
                "function": {
                  "arguments": "{\"cmd\":\"ls\"}",
                  "name": "bash"
                },
                "id": "c1",
                "type": "function"
              }
            ]
          },
          {
            "content": "a.txt\nb.txt",
            "role": "tool",
            "tool_call_id": "c1"
          },
          {
            "content": "Two files.",
            "role": "assistant"
          }
        ]
        "#);
    }

    #[test]
    fn parallel_tool_calls_share_assistant_message() {
        let items = vec![
            json!({"type": "message", "role": "user", "content": "stat both"}),
            json!({
                "type": "function_call",
                "call_id": "c1",
                "name": "stat",
                "arguments": "{\"p\":\"a\"}",
            }),
            json!({
                "type": "function_call",
                "call_id": "c2",
                "name": "stat",
                "arguments": "{\"p\":\"b\"}",
            }),
            json!({"type": "function_call_output", "call_id": "c1", "output": "ok-a"}),
            json!({"type": "function_call_output", "call_id": "c2", "output": "ok-b"}),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "stat both",
            "role": "user"
          },
          {
            "content": null,
            "role": "assistant",
            "tool_calls": [
              {
                "function": {
                  "arguments": "{\"p\":\"a\"}",
                  "name": "stat"
                },
                "id": "c1",
                "type": "function"
              },
              {
                "function": {
                  "arguments": "{\"p\":\"b\"}",
                  "name": "stat"
                },
                "id": "c2",
                "type": "function"
              }
            ]
          },
          {
            "content": "ok-a",
            "role": "tool",
            "tool_call_id": "c1"
          },
          {
            "content": "ok-b",
            "role": "tool",
            "tool_call_id": "c2"
          }
        ]
        "#);
    }

    #[test]
    fn reasoning_items_are_dropped() {
        let items = vec![
            json!({"type": "message", "role": "user", "content": "think hard"}),
            json!({
                "type": "reasoning",
                "id": "r1",
                "encrypted_content": "enc-blob",
                "summary": [{"type": "summary_text", "text": "secret thoughts"}],
            }),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "done"}],
            }),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "think hard",
            "role": "user"
          },
          {
            "content": "done",
            "role": "assistant"
          }
        ]
        "#);
    }

    #[test]
    fn empty_assistant_text_normalizes_to_null() {
        let items = vec![
            json!({"type": "message", "role": "user", "content": "hi"}),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": ""}],
            }),
            json!({
                "type": "function_call",
                "call_id": "c1",
                "name": "noop",
                "arguments": "{}",
            }),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "hi",
            "role": "user"
          },
          {
            "content": null,
            "role": "assistant",
            "tool_calls": [
              {
                "function": {
                  "arguments": "{}",
                  "name": "noop"
                },
                "id": "c1",
                "type": "function"
              }
            ]
          }
        ]
        "#);
    }

    #[test]
    fn multi_part_assistant_content_concatenates() {
        let items = vec![
            json!({"type": "message", "role": "user", "content": "hi"}),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "output_text", "text": "part-one "},
                    {"type": "text", "text": "part-two"},
                ],
            }),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "hi",
            "role": "user"
          },
          {
            "content": "part-one part-two",
            "role": "assistant"
          }
        ]
        "#);
    }

    #[test]
    fn roleless_message_item_is_treated_as_assistant() {
        // Matches nav's Responses parser convention: ResponseItem::Message
        // does not deserialize `role`, so raw_output items pushed back into
        // `input` may arrive without it.
        let items = vec![
            json!({"type": "message", "role": "user", "content": "ping"}),
            json!({
                "type": "message",
                "content": [{"type": "output_text", "text": "pong"}],
            }),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "ping",
            "role": "user"
          },
          {
            "content": "pong",
            "role": "assistant"
          }
        ]
        "#);
    }

    #[test]
    fn function_call_missing_call_id_is_skipped() {
        let items = vec![
            json!({"type": "message", "role": "user", "content": "go"}),
            json!({"type": "function_call", "name": "broken", "arguments": "{}"}),
            json!({
                "type": "function_call",
                "call_id": "c1",
                "name": "ok",
                "arguments": "{}",
            }),
            json!({"type": "function_call_output", "call_id": "c1", "output": "done"}),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "go",
            "role": "user"
          },
          {
            "content": null,
            "role": "assistant",
            "tool_calls": [
              {
                "function": {
                  "arguments": "{}",
                  "name": "ok"
                },
                "id": "c1",
                "type": "function"
              }
            ]
          },
          {
            "content": "done",
            "role": "tool",
            "tool_call_id": "c1"
          }
        ]
        "#);
    }

    #[test]
    fn function_call_output_missing_call_id_is_skipped() {
        let items = vec![
            json!({"type": "message", "role": "user", "content": "go"}),
            json!({"type": "function_call_output", "output": "orphan"}),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "go",
            "role": "user"
          }
        ]
        "#);
    }

    #[test]
    fn image_only_user_message_emits_placeholder() {
        let items = vec![json!({
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": ""},
                {"type": "input_image", "image_url": "data:image/png;base64,iVBORw0KG=="},
            ],
        })];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "[non-text attachment omitted]",
            "role": "user"
          }
        ]
        "#);
    }

    #[test]
    fn consecutive_assistant_messages_merge() {
        let items = vec![
            json!({"type": "message", "role": "user", "content": "go"}),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "alpha "}],
            }),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "beta"}],
            }),
        ];
        insta::assert_snapshot!(snap(&items), @r#"
        [
          {
            "content": "go",
            "role": "user"
          },
          {
            "content": "alpha beta",
            "role": "assistant"
          }
        ]
        "#);
    }
}
