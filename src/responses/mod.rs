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
