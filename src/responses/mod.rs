mod sse;
mod types;
mod websocket;

use crate::{
    auth::AuthConfig,
    cli::{Args, Transport},
    tools::tool_definitions,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::Path;
use types::{MessagePart, ResponseEnvelope, ResponseItem};

#[derive(Debug)]
pub(super) struct ToolCall {
    pub(super) call_id: String,
    pub(super) name: String,
    pub(super) arguments: Value,
}

pub(super) async fn create_response(
    client: &reqwest::Client,
    auth: &AuthConfig,
    args: &Args,
    cwd: &Path,
    input: &[Value],
) -> Result<ResponseEnvelope> {
    let body = response_body(args, cwd, input);
    match args.transport {
        Transport::Websocket => websocket::create_response_websocket(auth, body).await,
        Transport::Sse => sse::create_response_sse(client, auth, body).await,
    }
}

fn response_body(args: &Args, cwd: &Path, input: &[Value]) -> Value {
    // tools are just JSON descriptions. The model decides whether to emit
    // a function_call item; Rust remains responsible for actually doing work.
    json!({
        "model": args.model,
        "instructions": format!(
            "You are a small coding agent running in {}. Use tools to inspect, edit, search, and verify code. Prefer small, explicit steps. Paths must be relative.",
            cwd.display()
        ),
        "input": input,
        // store=false keeps the demo honest: nav manages the transcript itself,
        // and no server-side stored conversation is needed for the agent loop.
        "store": false,
        "tools": tool_definitions(),
    })
}

#[derive(Default)]
struct ResponseCollector {
    completed: Option<ResponseEnvelope>,
    output: Vec<ResponseItem>,
    raw_output: Vec<Value>,
}

impl ResponseCollector {
    fn push_event(&mut self, event: &Value, source: &str) -> Result<bool> {
        match event.get("type").and_then(Value::as_str) {
            Some("error") => bail!("{source} returned error: {event}"),
            Some("response.completed") => {
                self.completed = Some(decode_completed_response(event)?);
                return Ok(true);
            }
            Some("response.output_item.done") => {
                let item = event
                    .get("item")
                    .cloned()
                    .context("response.output_item.done event had no item")?;
                self.raw_output.push(item.clone());
                self.output.push(
                    serde_json::from_value::<ResponseItem>(item)
                        .context("failed to decode output item")?,
                );
            }
            _ => {}
        }
        Ok(false)
    }

    fn finish(self, source: &str) -> Result<ResponseEnvelope> {
        let mut completed = self
            .completed
            .with_context(|| format!("{source} ended without response.completed"))?;
        if completed.output.as_ref().is_none_or(Vec::is_empty) {
            completed.output = Some(self.output);
        }
        if completed.raw_output.is_empty() {
            completed.raw_output = self.raw_output;
        }
        Ok(completed)
    }
}

fn decode_completed_response(event: &Value) -> Result<ResponseEnvelope> {
    let response = event
        .get("response")
        .cloned()
        .context("response.completed event had no response")?;
    let mut envelope = serde_json::from_value::<ResponseEnvelope>(response.clone())
        .context("failed to decode completed response")?;
    envelope.raw_output = response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(envelope)
}

pub(super) fn process_response(response: &ResponseEnvelope) -> Result<Vec<ToolCall>> {
    let output = output_items(response)?;
    print_messages(output);
    function_calls(output)
}

fn print_messages(output: &[ResponseItem]) {
    // only message items are user-facing. Reasoning and tool-call items are
    // part of the loop, but printing them would make the CLI noisy.
    for item in output {
        if let ResponseItem::Message {
            content: Some(parts),
        } = item
        {
            for part in parts {
                match part {
                    MessagePart::OutputText { text } | MessagePart::Text { text } => {
                        println!("{text}");
                    }
                    MessagePart::Other => {}
                }
            }
        }
    }
}

fn function_calls(output: &[ResponseItem]) -> Result<Vec<ToolCall>> {
    // function_call arguments arrive as a JSON string. Parsing here gives
    // each local tool strongly shaped input before it touches the filesystem.
    output
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => Some((call_id, name, arguments)),
            _ => None,
        })
        .map(|(call_id, name, arguments)| {
            Ok(ToolCall {
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: serde_json::from_str(arguments)
                    .with_context(|| format!("failed to parse arguments for {name}"))?,
            })
        })
        .collect()
}

pub(super) fn into_raw_output(response: ResponseEnvelope) -> Vec<Value> {
    response.raw_output
}

fn output_items(response: &ResponseEnvelope) -> Result<&[ResponseItem]> {
    response
        .output
        .as_deref()
        .ok_or_else(|| anyhow!("Responses API returned no output"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Args, AuthMode, Transport};
    use serde_json::json;

    /// Minimal Args for unit-testing response_body and friends.
    fn test_args() -> Args {
        Args {
            model: "test-model".into(),
            auth: AuthMode::Chatgpt,
            transport: Transport::Websocket,
            codex_home: None,
            max_turns: 4,
            bash_timeout_secs: 10,
            prompt: vec!["test".into()],
        }
    }

    // ── response_body ─────────────────────────────────────────────

    #[test]
    fn response_body_includes_model_and_store_false() {
        let args = test_args();
        let cwd = std::path::Path::new("/tmp");
        let input = vec![json!({"type": "message", "role": "user", "content": "hi"})];
        let body = response_body(&args, cwd, &input);
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["store"], false);
        assert!(body["input"].is_array());
        assert!(body["tools"].is_array());
    }

    #[test]
    fn response_body_instructions_contain_cwd() {
        let args = test_args();
        let cwd = std::path::Path::new("/my/project");
        let body = response_body(&args, cwd, &[]);
        let instructions = body["instructions"].as_str().unwrap();
        assert!(instructions.contains("/my/project"));
    }

    #[test]
    fn response_body_passes_input_through() {
        let args = test_args();
        let cwd = std::path::Path::new("/tmp");
        let input = vec![
            json!({"type": "message", "role": "user", "content": "hello"}),
            json!({"type": "function_call_output", "call_id": "c1", "output": "ok"}),
        ];
        let body = response_body(&args, cwd, &input);
        let body_input = body["input"].as_array().unwrap();
        assert_eq!(body_input.len(), 2);
        assert_eq!(body_input[1]["call_id"], "c1");
    }

    // ── function_calls ────────────────────────────────────────────

    #[test]
    fn function_calls_extracts_single_call() {
        let items = vec![ResponseItem::FunctionCall {
            call_id: "c1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"foo.rs"}"#.into(),
        }];
        let calls = function_calls(&items).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "c1");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "foo.rs");
    }

    #[test]
    fn function_calls_extracts_multiple_calls() {
        let items = vec![
            ResponseItem::FunctionCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
            },
            ResponseItem::FunctionCall {
                call_id: "c2".into(),
                name: "bash".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            },
        ];
        let calls = function_calls(&items).unwrap();
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn function_calls_returns_empty_for_message_only() {
        let items = vec![ResponseItem::Message { content: None }];
        let calls = function_calls(&items).unwrap();
        assert!(calls.is_empty());
    }

    #[test]
    fn function_calls_returns_empty_for_other_items() {
        let items = vec![ResponseItem::Other];
        let calls = function_calls(&items).unwrap();
        assert!(calls.is_empty());
    }

    #[test]
    fn function_calls_rejects_invalid_arguments_json() {
        let items = vec![ResponseItem::FunctionCall {
            call_id: "c1".into(),
            name: "bash".into(),
            arguments: "not-json".into(),
        }];
        let err = function_calls(&items).unwrap_err();
        assert!(err.to_string().contains("failed to parse arguments"));
    }

    #[test]
    fn function_calls_skips_non_call_items_but_extracts_calls() {
        let items = vec![
            ResponseItem::Message { content: None },
            ResponseItem::FunctionCall {
                call_id: "c1".into(),
                name: "bash".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            },
            ResponseItem::Other,
        ];
        let calls = function_calls(&items).unwrap();
        assert_eq!(calls.len(), 1);
    }

    // ── process_response ──────────────────────────────────────────

    #[test]
    fn process_response_extracts_function_calls_from_envelope() {
        let response = ResponseEnvelope {
            output: Some(vec![ResponseItem::FunctionCall {
                call_id: "c1".into(),
                name: "bash".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            }]),
            raw_output: vec![],
        };
        let calls = process_response(&response).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
    }

    #[test]
    fn process_response_returns_empty_when_message_only() {
        let response = ResponseEnvelope {
            output: Some(vec![ResponseItem::Message {
                content: Some(vec![MessagePart::OutputText {
                    text: "done".into(),
                }]),
            }]),
            raw_output: vec![],
        };
        let calls = process_response(&response).unwrap();
        assert!(calls.is_empty());
    }

    #[test]
    fn process_response_errors_on_no_output() {
        let response = ResponseEnvelope {
            output: None,
            raw_output: vec![],
        };
        let err = process_response(&response).unwrap_err();
        assert!(err.to_string().contains("no output"));
    }

    // ── into_raw_output ───────────────────────────────────────────

    #[test]
    fn into_raw_output_returns_raw_items() {
        let response = ResponseEnvelope {
            output: Some(vec![]),
            raw_output: vec![json!({"type": "function_call", "call_id": "c1"})],
        };
        let raw = into_raw_output(response);
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0]["call_id"], "c1");
    }

    // ── decode_completed_response ─────────────────────────────────

    #[test]
    fn decode_completed_response_parses_envelope() {
        let event = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_123",
                "output": [
                    {"type": "message", "content": [{"type": "output_text", "text": "hi"}]}
                ]
            }
        });
        let envelope = decode_completed_response(&event).unwrap();
        assert!(envelope.output.is_some());
        assert_eq!(envelope.raw_output.len(), 1);
    }

    #[test]
    fn decode_completed_response_errors_on_missing_response_field() {
        let event = json!({"type": "response.completed"});
        let err = decode_completed_response(&event).unwrap_err();
        assert!(err.to_string().contains("no response"));
    }

    // ── ResponseCollector ─────────────────────────────────────────

    #[test]
    fn collector_error_event_bails() {
        let mut collector = ResponseCollector::default();
        let event = json!({"type": "error", "message": "boom"});
        let err = collector.push_event(&event, "source").unwrap_err();
        assert!(err.to_string().contains("source returned error"));
    }

    #[test]
    fn collector_completed_event_returns_true() {
        let mut collector = ResponseCollector::default();
        let event = json!({
            "type": "response.completed",
            "response": {"output": []}
        });
        let done = collector.push_event(&event, "src").unwrap();
        assert!(done);
    }

    #[test]
    fn collector_output_item_done_appends_to_output() {
        let mut collector = ResponseCollector::default();
        let event = json!({
            "type": "response.output_item.done",
            "item": {"type": "message", "content": null}
        });
        let done = collector.push_event(&event, "src").unwrap();
        assert!(!done);
        assert_eq!(collector.output.len(), 1);
        assert_eq!(collector.raw_output.len(), 1);
    }

    #[test]
    fn collector_ignores_unknown_events() {
        let mut collector = ResponseCollector::default();
        let event = json!({"type": "response.created"});
        let done = collector.push_event(&event, "src").unwrap();
        assert!(!done);
        assert!(collector.output.is_empty());
    }

    #[test]
    fn collector_finish_errors_without_completed() {
        let collector = ResponseCollector::default();
        let err = collector.finish("test").unwrap_err();
        assert!(err.to_string().contains("test ended without response.completed"));
    }

    #[test]
    fn collector_finish_merges_streamed_output() {
        let mut collector = ResponseCollector::default();
        // Simulate receiving a completed response with no output field
        let completed_event = json!({
            "type": "response.completed",
            "response": {"id": "r1"}
        });
        collector.push_event(&completed_event, "src").unwrap();
        // Simulate receiving an output item (arrived before completed but not in the response object)
        let output_event = json!({
            "type": "response.output_item.done",
            "item": {"type": "message", "content": [{"type": "output_text", "text": "hi"}]}
        });
        collector.push_event(&output_event, "src").unwrap();

        let result = collector.finish("src").unwrap();
        let output = result.output.as_ref().unwrap();
        assert_eq!(output.len(), 1);
    }

    #[test]
    fn collector_finish_preserves_completed_output() {
        let mut collector = ResponseCollector::default();
        let completed_event = json!({
            "type": "response.completed",
            "response": {
                "output": [{"type": "message", "content": [{"type": "text", "text": "kept"}]}]
            }
        });
        collector.push_event(&completed_event, "src").unwrap();

        let result = collector.finish("src").unwrap();
        let output = result.output.as_ref().unwrap();
        assert_eq!(output.len(), 1);
        // The output from completed response is preserved, not replaced by empty streamed output
        assert!(matches!(&output[0], ResponseItem::Message { content } if content.is_some()));
    }
}
