//! Decoder trait: provider response/envelope → canonical turns.

use std::collections::HashMap;

use nav_types::{ArtifactId, MessageId, ProviderPayloadId, RunId, ToolCallId};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};

use crate::models::ApiKind;
use crate::sessions::{DecodeStatus, Part, RawJson, TokenUsage, Turn, TurnMeta, TurnRole};

/// Converts a provider-specific response into canonical model output.
///
/// Implementations decide how to extract assistant text, tool calls, and
/// other turn-level data from whatever envelope the provider returns.
pub trait Decoder {
    type Response;
    type Output;
    type Error;

    fn decode(&self, response: &Self::Response) -> Result<Self::Output, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiChatCompletionsDecodeInput {
    pub provider_payload_id: ProviderPayloadId,
    pub raw_artifact_id: ArtifactId,
    pub run_id: RunId,
    pub provider_id: Option<String>,
    pub raw_json: Vec<u8>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiResponsesDecodeInput {
    pub provider_payload_id: ProviderPayloadId,
    pub raw_artifact_id: ArtifactId,
    pub run_id: RunId,
    pub provider_id: Option<String>,
    pub raw_json: Vec<u8>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedProviderPayload {
    pub status: DecodeStatus,
    pub turns: Vec<DecodedTurn>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedTurn {
    pub turn: Turn,
    pub parts: Vec<DecodedPart>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedPart {
    pub part: Part,
    pub provider_payload_id: ProviderPayloadId,
    pub provider_json_pointer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    MalformedJson(String),
    MalformedResponse(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedJson(message) => write!(formatter, "malformed JSON: {message}"),
            Self::MalformedResponse(message) => write!(formatter, "malformed response: {message}"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[derive(Debug, Clone, Default)]
pub struct OpenAiChatCompletionsDecoder;

impl OpenAiChatCompletionsDecoder {
    pub fn new() -> Self {
        Self
    }
}

impl Decoder for OpenAiChatCompletionsDecoder {
    type Response = OpenAiChatCompletionsDecodeInput;
    type Output = DecodedProviderPayload;
    type Error = DecodeError;

    fn decode(&self, response: &Self::Response) -> Result<Self::Output, Self::Error> {
        decode_openai_chat_completions(response)
    }
}

#[derive(Debug, Clone, Default)]
pub struct OpenAiResponsesDecoder;

impl OpenAiResponsesDecoder {
    pub fn new() -> Self {
        Self
    }
}

impl Decoder for OpenAiResponsesDecoder {
    type Response = OpenAiResponsesDecodeInput;
    type Output = DecodedProviderPayload;
    type Error = DecodeError;

    fn decode(&self, response: &Self::Response) -> Result<Self::Output, Self::Error> {
        decode_openai_responses(response)
    }
}

fn decode_openai_responses(
    input: &OpenAiResponsesDecodeInput,
) -> Result<DecodedProviderPayload, DecodeError> {
    let value: Value = serde_json::from_slice(&input.raw_json)
        .map_err(|error| DecodeError::MalformedJson(error.to_string()))?;
    let raw_response = serde_json::from_slice::<RawOpenAiResponsesResponse>(&input.raw_json).ok();
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| DecodeError::MalformedResponse("missing output array".to_string()))?;

    let model_id = value
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let usage = decode_responses_usage(value.get("usage"));

    let mut parts = Vec::new();
    for (index, item) in output.iter().enumerate() {
        let pointer = format!("/output/{index}");
        parts.push(responses_step_start_part(input, &pointer));
        parts.extend(decode_responses_output_item(
            input,
            index,
            item,
            raw_responses_item(raw_response.as_ref(), index),
        )?);
        parts.push(responses_step_finish_part(
            input,
            &pointer,
            item,
            status.as_deref(),
            responses_step_tokens(index, output.len(), usage.as_ref()),
        ));
    }

    let turn = DecodedTurn {
        turn: Turn {
            id: derived_message_id(input.provider_payload_id.as_str(), "response"),
            run_id: input.run_id.clone(),
            seq: 0,
            role: TurnRole::Assistant,
            meta: TurnMeta {
                model_provider: input.provider_id.clone(),
                model_id,
                api_kind: Some(ApiKind::OpenAiResponses),
                finish_reason: status,
                usage,
                parent_id: None,
            },
            created_at: input.created_at,
        },
        parts,
    };

    let status = if decoded_turn_has_unknowns(&turn) {
        DecodeStatus::DecodedWithUnknowns
    } else {
        DecodeStatus::Decoded
    };

    Ok(DecodedProviderPayload {
        status,
        turns: vec![turn],
    })
}

fn decode_responses_output_item(
    input: &OpenAiResponsesDecodeInput,
    index: usize,
    item: &Value,
    raw_item: Option<&RawValue>,
) -> Result<Vec<DecodedPart>, DecodeError> {
    match item.get("type").and_then(Value::as_str) {
        Some("message") => decode_responses_message(input, index, item),
        Some("function_call") => decode_responses_function_call(input, index, item),
        Some("function_call_output") => decode_responses_function_call_output(input, index, item),
        Some("reasoning") => decode_responses_reasoning(input, index, item),
        unknown_type => {
            decode_unknown_responses_output_item(input, index, item, raw_item, unknown_type)
        }
    }
}

fn decode_unknown_responses_output_item(
    input: &OpenAiResponsesDecodeInput,
    index: usize,
    item: &Value,
    raw_item: Option<&RawValue>,
    item_type: Option<&str>,
) -> Result<Vec<DecodedPart>, DecodeError> {
    let kind = format!("response.output_item.{}", item_type.unwrap_or("unknown"));
    Ok(vec![responses_provider_opaque_part(
        input,
        kind,
        format!("/output/{index}"),
        raw_responses_item_payload(raw_item, item)?,
    )])
}

fn decode_responses_function_call(
    input: &OpenAiResponsesDecodeInput,
    output_index: usize,
    item: &Value,
) -> Result<Vec<DecodedPart>, DecodeError> {
    let pointer = format!("/output/{output_index}");
    let call_id = responses_call_id(item, &pointer)?;
    let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
        DecodeError::MalformedResponse(format!("missing function call name at {pointer}"))
    })?;
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .map(parse_tool_arguments)
        .transpose()?
        .unwrap_or(Value::Null);

    Ok(vec![DecodedPart {
        part: Part::ToolCall {
            id: derived_responses_tool_call_id(input.provider_payload_id.as_str(), call_id),
            name: name.to_string(),
            arguments,
            raw_arguments_artifact_id: None,
        },
        provider_payload_id: input.provider_payload_id.clone(),
        provider_json_pointer: pointer,
    }])
}

fn decode_responses_function_call_output(
    input: &OpenAiResponsesDecodeInput,
    output_index: usize,
    item: &Value,
) -> Result<Vec<DecodedPart>, DecodeError> {
    let pointer = format!("/output/{output_index}");
    let call_id = responses_call_id(item, &pointer)?;
    let output = item.get("output").ok_or_else(|| {
        DecodeError::MalformedResponse(format!("missing function call output at {pointer}"))
    })?;

    Ok(vec![DecodedPart {
        part: Part::ToolResult {
            call_id: derived_responses_tool_call_id(input.provider_payload_id.as_str(), call_id),
            content: response_output_content(output),
            raw_artifact_id: None,
            is_error: item
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status == "failed" || status == "incomplete"),
        },
        provider_payload_id: input.provider_payload_id.clone(),
        provider_json_pointer: format!("{pointer}/output"),
    }])
}

fn decode_responses_reasoning(
    input: &OpenAiResponsesDecodeInput,
    output_index: usize,
    item: &Value,
) -> Result<Vec<DecodedPart>, DecodeError> {
    let mut parts = Vec::new();

    if let Some(encrypted_content) = item.get("encrypted_content").and_then(Value::as_str) {
        parts.push(responses_thinking_part(
            input,
            encrypted_content,
            "encrypted",
            format!("/output/{output_index}/encrypted_content"),
        ));
    }

    parts.extend(decode_responses_reasoning_text_items(
        input,
        output_index,
        item,
        "content",
        "reasoning_text",
    )?);
    parts.extend(decode_responses_reasoning_text_items(
        input,
        output_index,
        item,
        "summary",
        "summary_text",
    )?);

    Ok(parts)
}

fn responses_call_id<'a>(item: &'a Value, pointer: &str) -> Result<&'a str, DecodeError> {
    item.get("call_id").and_then(Value::as_str).ok_or_else(|| {
        DecodeError::MalformedResponse(format!("missing function call_id at {pointer}"))
    })
}

fn response_output_content(output: &Value) -> String {
    output
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| output.to_string())
}

fn decode_responses_reasoning_text_items(
    input: &OpenAiResponsesDecodeInput,
    output_index: usize,
    item: &Value,
    field: &str,
    expected_type: &str,
) -> Result<Vec<DecodedPart>, DecodeError> {
    let Some(items) = item.get(field).and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    let mut parts = Vec::new();
    for (item_index, text_item) in items.iter().enumerate() {
        let pointer = format!("/output/{output_index}/{field}/{item_index}");
        if text_item.get("type").and_then(Value::as_str) == Some(expected_type) {
            let text = text_item
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    DecodeError::MalformedResponse(format!("missing reasoning text at {pointer}"))
                })?;
            parts.push(responses_thinking_part(
                input,
                text,
                expected_type,
                format!("{pointer}/text"),
            ));
        } else {
            parts.push(responses_provider_opaque_part(
                input,
                format!("response.reasoning_{field}.unknown"),
                pointer,
                raw_json_from_value(text_item)?,
            ));
        }
    }

    Ok(parts)
}

fn responses_thinking_part(
    input: &OpenAiResponsesDecodeInput,
    text: &str,
    provider_hint: &str,
    provider_json_pointer: String,
) -> DecodedPart {
    DecodedPart {
        part: Part::Thinking {
            text: text.to_string(),
            provider_hint: Some(provider_hint.to_string()),
        },
        provider_payload_id: input.provider_payload_id.clone(),
        provider_json_pointer,
    }
}

fn decode_responses_message(
    input: &OpenAiResponsesDecodeInput,
    output_index: usize,
    item: &Value,
) -> Result<Vec<DecodedPart>, DecodeError> {
    let content = item
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            DecodeError::MalformedResponse(format!(
                "missing message content array at /output/{output_index}"
            ))
        })?;

    let mut parts = Vec::new();
    for (content_index, content_item) in content.iter().enumerate() {
        match content_item.get("type").and_then(Value::as_str) {
            Some("output_text") => {
                let text = content_item
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        DecodeError::MalformedResponse(format!(
                            "missing output_text text at /output/{output_index}/content/{content_index}"
                        ))
                    })?;
                parts.push(DecodedPart {
                    part: Part::Text {
                        text: text.to_string(),
                        synthetic: None,
                    },
                    provider_payload_id: input.provider_payload_id.clone(),
                    provider_json_pointer: format!(
                        "/output/{output_index}/content/{content_index}/text"
                    ),
                });
            }
            Some(content_type) => {
                parts.push(responses_provider_opaque_part(
                    input,
                    format!("response.message_content.{content_type}"),
                    format!("/output/{output_index}/content/{content_index}"),
                    raw_json_from_value(content_item)?,
                ));
            }
            None => {
                parts.push(responses_provider_opaque_part(
                    input,
                    "response.message_content.unknown".to_string(),
                    format!("/output/{output_index}/content/{content_index}"),
                    raw_json_from_value(content_item)?,
                ));
            }
        }
    }
    Ok(parts)
}

fn responses_step_reason(item: &Value, response_status: Option<&str>) -> String {
    item.get("status")
        .and_then(Value::as_str)
        .or(response_status)
        .unwrap_or("completed")
        .to_string()
}

fn responses_step_start_part(input: &OpenAiResponsesDecodeInput, pointer: &str) -> DecodedPart {
    DecodedPart {
        part: Part::StepStart { snapshot: None },
        provider_payload_id: input.provider_payload_id.clone(),
        provider_json_pointer: pointer.to_string(),
    }
}

fn responses_step_finish_part(
    input: &OpenAiResponsesDecodeInput,
    pointer: &str,
    item: &Value,
    response_status: Option<&str>,
    tokens: TokenUsage,
) -> DecodedPart {
    DecodedPart {
        part: Part::StepFinish {
            reason: responses_step_reason(item, response_status),
            cost: 0.0,
            tokens,
            snapshot: None,
        },
        provider_payload_id: input.provider_payload_id.clone(),
        provider_json_pointer: pointer.to_string(),
    }
}

fn responses_step_tokens(
    index: usize,
    output_len: usize,
    usage: Option<&TokenUsage>,
) -> TokenUsage {
    if index + 1 == output_len {
        return usage.cloned().unwrap_or_default();
    }

    TokenUsage::default()
}

fn responses_provider_opaque_part(
    input: &OpenAiResponsesDecodeInput,
    kind: String,
    provider_json_pointer: String,
    raw_payload: RawJson,
) -> DecodedPart {
    DecodedPart {
        part: Part::ProviderOpaque {
            api_kind: ApiKind::OpenAiResponses,
            kind,
            raw_artifact_id: input.raw_artifact_id.clone(),
            raw_payload: Some(raw_payload),
        },
        provider_payload_id: input.provider_payload_id.clone(),
        provider_json_pointer,
    }
}

fn raw_responses_item_payload(
    raw_item: Option<&RawValue>,
    item: &Value,
) -> Result<RawJson, DecodeError> {
    raw_item
        .map(|raw| raw_json_from_str(raw.get()))
        .unwrap_or_else(|| raw_json_from_value(item))
}

fn raw_responses_item(
    raw_response: Option<&RawOpenAiResponsesResponse>,
    index: usize,
) -> Option<&RawValue> {
    raw_response
        .and_then(|response| response.output.get(index))
        .map(Box::as_ref)
}

fn decode_responses_usage(value: Option<&Value>) -> Option<TokenUsage> {
    let usage = value?;
    Some(TokenUsage {
        input: optional_u64_field(usage, "input_tokens"),
        output: optional_u64_field(usage, "output_tokens"),
        reasoning: nested_optional_u64_field(usage, "output_tokens_details", "reasoning_tokens"),
        cache_read: nested_optional_u64_field(usage, "input_tokens_details", "cached_tokens"),
        cache_write: 0,
    })
}

fn decode_openai_chat_completions(
    input: &OpenAiChatCompletionsDecodeInput,
) -> Result<DecodedProviderPayload, DecodeError> {
    let value: Value = serde_json::from_slice(&input.raw_json)
        .map_err(|error| DecodeError::MalformedJson(error.to_string()))?;
    let raw_response = serde_json::from_slice::<RawChatCompletionResponse>(&input.raw_json).ok();
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| DecodeError::MalformedResponse("missing choices array".to_string()))?;

    let model_id = value
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let usage = decode_usage(value.get("usage"));

    let mut turns = choices
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            decode_choice(
                input,
                index,
                choice,
                raw_response
                    .as_ref()
                    .and_then(|response| response.choices.get(index)),
                model_id.clone(),
                usage.clone(),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Decode unmapped top-level response fields (gateway extras like
    // system_fingerprint, service_tier, provider-specific metadata).
    let response_extras = decode_unmapped_response_fields(input, &value)?;
    if !response_extras.is_empty() {
        if let Some(first) = turns.first_mut() {
            first.parts.extend(response_extras);
        } else {
            // No choices — synthesize a turn so extras are not silently dropped.
            turns.push(DecodedTurn {
                turn: Turn {
                    id: derived_message_id(input.provider_payload_id.as_str(), "response_extras"),
                    run_id: input.run_id.clone(),
                    seq: 0,
                    role: TurnRole::Assistant,
                    meta: TurnMeta {
                        model_provider: input.provider_id.clone(),
                        model_id: model_id.clone(),
                        api_kind: Some(ApiKind::OpenAiCompletions),
                        ..TurnMeta::default()
                    },
                    created_at: input.created_at,
                },
                parts: response_extras,
            });
        }
    }

    let status = if turns.iter().any(decoded_turn_has_unknowns) {
        DecodeStatus::DecodedWithUnknowns
    } else {
        DecodeStatus::Decoded
    };

    Ok(DecodedProviderPayload { status, turns })
}

fn decode_choice(
    input: &OpenAiChatCompletionsDecodeInput,
    index: usize,
    choice: &Value,
    raw_choice: Option<&RawChatCompletionChoice>,
    model_id: Option<String>,
    usage: Option<TokenUsage>,
) -> Result<DecodedTurn, DecodeError> {
    let message = choice.get("message").ok_or_else(|| {
        DecodeError::MalformedResponse(format!("missing message for choice {index}"))
    })?;
    if !message.is_object() {
        return Err(DecodeError::MalformedResponse(format!(
            "message for choice {index} is not an object"
        )));
    }

    let mut parts = Vec::new();

    if let Some(text) = message.get("content").and_then(Value::as_str) {
        parts.push(DecodedPart {
            part: Part::Text {
                text: text.to_string(),
                synthetic: None,
            },
            provider_payload_id: input.provider_payload_id.clone(),
            provider_json_pointer: format!("/choices/{index}/message/content"),
        });
    }

    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (tool_index, tool_call) in tool_calls.iter().enumerate() {
            parts.push(decode_tool_call(
                input, index, tool_index, tool_call, raw_choice,
            )?);
        }
    }

    parts.extend(decode_unmapped_message_fields(
        input, index, message, raw_choice,
    )?);

    Ok(DecodedTurn {
        turn: Turn {
            id: derived_message_id(
                input.provider_payload_id.as_str(),
                &format!("choice:{index}"),
            ),
            run_id: input.run_id.clone(),
            seq: index as u32,
            role: TurnRole::Assistant,
            meta: TurnMeta {
                model_provider: input.provider_id.clone(),
                model_id,
                api_kind: Some(ApiKind::OpenAiCompletions),
                finish_reason: choice
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                usage,
                parent_id: None,
            },
            created_at: input.created_at,
        },
        parts,
    })
}

fn decode_unmapped_message_fields(
    input: &OpenAiChatCompletionsDecodeInput,
    choice_index: usize,
    message: &Value,
    raw_choice: Option<&RawChatCompletionChoice>,
) -> Result<Vec<DecodedPart>, DecodeError> {
    let Some(fields) = message.as_object() else {
        return Ok(Vec::new());
    };

    let mut parts = Vec::new();
    for (name, payload) in fields {
        if chat_message_field_is_mapped(name, payload) {
            continue;
        }

        let pointer = format!(
            "/choices/{choice_index}/message/{}",
            json_pointer_token(name)
        );
        let raw_payload = raw_message_field_payload(raw_choice, name)
            .unwrap_or_else(|| raw_json_from_value(payload));
        parts.push(DecodedPart {
            part: provider_opaque_part(input, format!("message.{name}"), raw_payload?),
            provider_payload_id: input.provider_payload_id.clone(),
            provider_json_pointer: pointer,
        });
    }
    Ok(parts)
}

fn decode_unmapped_response_fields(
    input: &OpenAiChatCompletionsDecodeInput,
    response: &Value,
) -> Result<Vec<DecodedPart>, DecodeError> {
    let Some(fields) = response.as_object() else {
        return Ok(Vec::new());
    };

    let mut parts = Vec::new();
    for (name, payload) in fields {
        if chat_response_field_is_mapped(name) {
            continue;
        }

        let pointer = format!("/{}", json_pointer_token(name));
        let raw_payload = raw_json_from_value(payload)?;
        parts.push(DecodedPart {
            part: provider_opaque_part(input, format!("response.{name}"), raw_payload),
            provider_payload_id: input.provider_payload_id.clone(),
            provider_json_pointer: pointer,
        });
    }
    Ok(parts)
}

fn chat_response_field_is_mapped(name: &str) -> bool {
    matches!(
        name,
        "id" | "object" | "created" | "model" | "choices" | "usage"
    )
}

fn chat_message_field_is_mapped(name: &str, payload: &Value) -> bool {
    match name {
        "role" => payload.is_string(),
        "content" => payload.is_string() || payload.is_null(),
        "tool_calls" => payload.is_array() || payload.is_null(),
        "function_call" => payload.is_null(),
        _ => false,
    }
}

fn json_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn raw_json_from_value(value: &Value) -> Result<RawJson, DecodeError> {
    RawJson::from_string(value.to_string()).map_err(|error| {
        DecodeError::MalformedResponse(format!("failed to preserve unknown JSON payload: {error}"))
    })
}

fn decoded_turn_has_unknowns(turn: &DecodedTurn) -> bool {
    turn.parts
        .iter()
        .any(|part| matches!(part.part, Part::ProviderOpaque { .. }))
}

fn decode_tool_call(
    input: &OpenAiChatCompletionsDecodeInput,
    choice_index: usize,
    tool_index: usize,
    tool_call: &Value,
    raw_choice: Option<&RawChatCompletionChoice>,
) -> Result<DecodedPart, DecodeError> {
    let pointer = format!("/choices/{choice_index}/message/tool_calls/{tool_index}");
    let tool_type = tool_call
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if tool_type != "function" || !tool_call.get("function").is_some_and(Value::is_object) {
        let raw_payload = raw_tool_call_payload(raw_choice, tool_index)
            .unwrap_or_else(|| raw_json_from_value(tool_call));
        return Ok(DecodedPart {
            part: provider_opaque_part(input, format!("tool_call.{tool_type}"), raw_payload?),
            provider_payload_id: input.provider_payload_id.clone(),
            provider_json_pointer: pointer,
        });
    }

    let function = tool_call
        .get("function")
        .expect("function object checked above");
    let name = function
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            DecodeError::MalformedResponse(format!("missing function name at {pointer}"))
        })?;
    let arguments = function
        .get("arguments")
        .and_then(Value::as_str)
        .map(parse_tool_arguments)
        .transpose()?
        .unwrap_or(Value::Null);

    Ok(DecodedPart {
        part: Part::ToolCall {
            id: derived_tool_call_id(input.provider_payload_id.as_str(), &pointer),
            name: name.to_string(),
            arguments,
            raw_arguments_artifact_id: None,
        },
        provider_payload_id: input.provider_payload_id.clone(),
        provider_json_pointer: pointer,
    })
}

fn provider_opaque_part(
    input: &OpenAiChatCompletionsDecodeInput,
    kind: String,
    raw_payload: RawJson,
) -> Part {
    Part::ProviderOpaque {
        api_kind: ApiKind::OpenAiCompletions,
        kind,
        raw_artifact_id: input.raw_artifact_id.clone(),
        raw_payload: Some(raw_payload),
    }
}

fn raw_message_field_payload(
    raw_choice: Option<&RawChatCompletionChoice>,
    name: &str,
) -> Option<Result<RawJson, DecodeError>> {
    Some(
        raw_message_field(raw_choice?, name)
            .and_then(|raw_payload| raw_json_from_str(raw_payload.get())),
    )
}

fn raw_tool_call_payload(
    raw_choice: Option<&RawChatCompletionChoice>,
    tool_index: usize,
) -> Option<Result<RawJson, DecodeError>> {
    Some(
        raw_message_field(raw_choice?, "tool_calls").and_then(|raw_tool_calls| {
            raw_tool_call_array_element(raw_tool_calls.get(), tool_index)
        }),
    )
}

fn raw_message_field(
    raw_choice: &RawChatCompletionChoice,
    name: &str,
) -> Result<Box<RawValue>, DecodeError> {
    let mut fields = serde_json::from_str::<HashMap<String, Box<RawValue>>>(
        raw_choice.message.get(),
    )
    .map_err(|error| {
        DecodeError::MalformedResponse(format!("failed to preserve raw message field: {error}"))
    })?;
    fields.remove(name).ok_or_else(|| {
        DecodeError::MalformedResponse(format!("missing raw message field `{name}`"))
    })
}

fn raw_json_from_str(raw: &str) -> Result<RawJson, DecodeError> {
    RawJson::from_string(raw.to_string()).map_err(|error| {
        DecodeError::MalformedResponse(format!("failed to preserve unknown JSON payload: {error}"))
    })
}

fn raw_tool_call_array_element(array: &str, tool_index: usize) -> Result<RawJson, DecodeError> {
    let tool_calls = serde_json::from_str::<Vec<Box<RawValue>>>(array).map_err(|error| {
        DecodeError::MalformedResponse(format!("failed to preserve raw tool calls array: {error}"))
    })?;
    let raw_payload = tool_calls.get(tool_index).ok_or_else(|| {
        DecodeError::MalformedResponse(format!(
            "failed to preserve raw tool call payload at index {tool_index}"
        ))
    })?;
    raw_json_from_str(raw_payload.get())
}

fn parse_tool_arguments(arguments: &str) -> Result<Value, DecodeError> {
    serde_json::from_str(arguments).map_err(|error| {
        DecodeError::MalformedResponse(format!("tool call arguments are not JSON: {error}"))
    })
}

fn decode_usage(value: Option<&Value>) -> Option<TokenUsage> {
    let usage = value?;
    Some(TokenUsage {
        input: optional_u64_field(usage, "prompt_tokens"),
        output: optional_u64_field(usage, "completion_tokens"),
        reasoning: nested_optional_u64_field(
            usage,
            "completion_tokens_details",
            "reasoning_tokens",
        ),
        cache_read: nested_optional_u64_field(usage, "prompt_tokens_details", "cached_tokens"),
        cache_write: 0,
    })
}

fn optional_u64_field(value: &Value, field: &str) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or_default()
}

fn nested_optional_u64_field(value: &Value, object_field: &str, number_field: &str) -> u64 {
    value
        .get(object_field)
        .and_then(|nested| nested.get(number_field))
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn derived_message_id(payload_id: &str, pointer: &str) -> MessageId {
    MessageId::new_unchecked(derived_uuid_v7_string(payload_id, pointer))
}

fn derived_tool_call_id(payload_id: &str, pointer: &str) -> ToolCallId {
    ToolCallId::new_unchecked(derived_uuid_v7_string(payload_id, pointer))
}

fn derived_responses_tool_call_id(payload_id: &str, call_id: &str) -> ToolCallId {
    ToolCallId::new_unchecked(derived_uuid_v7_string(
        payload_id,
        &format!("responses_call:{call_id}"),
    ))
}

fn derived_uuid_v7_string(payload_id: &str, pointer: &str) -> String {
    let hash = stable_hash64(payload_id, pointer);
    format!(
        "019f2f6f-f178-7a72-9f28-{:012x}",
        hash & 0x0000_ffff_ffff_ffff
    )
}

fn stable_hash64(payload_id: &str, pointer: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    for bytes in [payload_id.as_bytes(), b"\0", pointer.as_bytes()] {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

#[derive(Deserialize)]
struct RawChatCompletionResponse {
    choices: Vec<RawChatCompletionChoice>,
}

#[derive(Deserialize)]
struct RawChatCompletionChoice {
    message: Box<RawValue>,
}

#[derive(Deserialize)]
struct RawOpenAiResponsesResponse {
    output: Vec<Box<RawValue>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::openai_completions::{
        ChatCompletionChoice, ChatCompletionResponse, ChatCompletionResponseMessage,
    };
    use crate::sessions::ModelTurn;

    struct OpenAiDecoder;

    impl Decoder for OpenAiDecoder {
        type Response = ChatCompletionResponse;
        type Output = Vec<ModelTurn>;
        type Error = std::convert::Infallible;

        fn decode(&self, response: &Self::Response) -> Result<Self::Output, Self::Error> {
            Ok(response
                .choices
                .iter()
                .map(|choice| {
                    ModelTurn::assistant_text(choice.message.content.clone().unwrap_or_default())
                })
                .collect())
        }
    }

    #[test]
    fn openai_decoder_extracts_assistant_text_from_response() {
        let decoder = OpenAiDecoder;
        let response = ChatCompletionResponse {
            id: None,
            model: None,
            choices: vec![ChatCompletionChoice {
                index: None,
                message: ChatCompletionResponseMessage {
                    role: Some("assistant".to_string()),
                    content: Some("hello there".to_string()),
                },
                finish_reason: None,
            }],
            usage: None,
        };

        let turns = decoder.decode(&response).unwrap();

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].text_content(), "hello there");
    }

    #[test]
    fn openai_decoder_handles_empty_choices() {
        let decoder = OpenAiDecoder;
        let response = ChatCompletionResponse {
            id: None,
            model: None,
            choices: vec![],
            usage: None,
        };

        let turns = decoder.decode(&response).unwrap();

        assert!(turns.is_empty());
    }
}
