use crate::agent::TurnUsage;
use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use super::types::{MessagePart, RawUsage, ResponseEnvelope, ResponseItem};

#[derive(Debug)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

pub fn process_response(response: &ResponseEnvelope) -> Result<Vec<ToolCall>> {
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

pub(super) fn function_calls(output: &[ResponseItem]) -> Result<Vec<ToolCall>> {
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

/// Convenience wrapper used by the agent loop. Skips the stdout side-effect of
/// `process_response` so events stay the single source of truth for output.
pub(crate) fn function_calls_from(response: &ResponseEnvelope) -> Result<Vec<ToolCall>> {
    let output = output_items(response)?;
    function_calls(output)
}

pub fn into_raw_output(response: ResponseEnvelope) -> Vec<Value> {
    response.raw_output
}

/// Normalizes the OpenAI Responses `usage` object into [`TurnUsage`].
///
/// Maps `input_tokens` -> `tokens_input`,
/// `output_tokens` -> `tokens_output`,
/// `input_tokens_details.cached_tokens` -> `tokens_input_cached`,
/// `output_tokens_details.reasoning_tokens` -> `tokens_reasoning`.
/// Any missing field defaults to 0.
pub(crate) fn turn_usage_from(response: &ResponseEnvelope) -> TurnUsage {
    let Some(usage) = response.usage.as_ref() else {
        return TurnUsage::default();
    };
    usage_from_raw(usage)
}

fn usage_from_raw(usage: &RawUsage) -> TurnUsage {
    TurnUsage {
        tokens_input: usage.input_tokens.unwrap_or(0),
        tokens_output: usage.output_tokens.unwrap_or(0),
        tokens_input_cached: usage
            .input_tokens_details
            .as_ref()
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0),
        tokens_reasoning: usage
            .output_tokens_details
            .as_ref()
            .and_then(|d| d.reasoning_tokens)
            .unwrap_or(0),
    }
}

fn output_items(response: &ResponseEnvelope) -> Result<&[ResponseItem]> {
    response
        .output
        .as_deref()
        .ok_or_else(|| anyhow!("Responses API returned no output"))
}
