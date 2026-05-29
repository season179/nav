//! Fixture-driven tests for the OpenAI Chat Completions canonical decoder.

use nav_harness::models::{
    ApiKind, Decoder, OpenAiChatCompletionsDecodeInput, OpenAiChatCompletionsDecoder,
    OpenAiResponsesDecodeInput, OpenAiResponsesDecoder,
};
use nav_harness::sessions::{DecodeStatus, Part, TokenUsage, TurnRole};
use nav_types::{ArtifactId, ProviderPayloadId, RunId, ToolCallId};

fn provider_payload_id() -> ProviderPayloadId {
    ProviderPayloadId::try_new("pay_0000018bcfe56800_0000000000000001")
        .expect("test provider payload id")
}

fn artifact_id() -> ArtifactId {
    ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").expect("test artifact id")
}

fn run_id() -> RunId {
    RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").expect("test run id")
}

fn decode(raw_json: &str) -> nav_harness::models::DecodedProviderPayload {
    let decoder = OpenAiChatCompletionsDecoder::new();
    decoder
        .decode(&OpenAiChatCompletionsDecodeInput {
            provider_payload_id: provider_payload_id(),
            raw_artifact_id: artifact_id(),
            run_id: run_id(),
            provider_id: Some("openai".to_string()),
            raw_json: raw_json.as_bytes().to_vec(),
            created_at: 1_700_000_000_000,
        })
        .expect("fixture should decode")
}

fn decode_err(raw_json: &str) -> nav_harness::models::DecodeError {
    let decoder = OpenAiChatCompletionsDecoder::new();
    decoder
        .decode(&OpenAiChatCompletionsDecodeInput {
            provider_payload_id: provider_payload_id(),
            raw_artifact_id: artifact_id(),
            run_id: run_id(),
            provider_id: Some("openai".to_string()),
            raw_json: raw_json.as_bytes().to_vec(),
            created_at: 1_700_000_000_000,
        })
        .expect_err("fixture should fail")
}

fn decode_responses(raw_json: &str) -> nav_harness::models::DecodedProviderPayload {
    let decoder = OpenAiResponsesDecoder::new();
    decoder
        .decode(&OpenAiResponsesDecodeInput {
            provider_payload_id: provider_payload_id(),
            raw_artifact_id: artifact_id(),
            run_id: run_id(),
            provider_id: Some("openai".to_string()),
            raw_json: raw_json.as_bytes().to_vec(),
            created_at: 1_700_000_000_000,
        })
        .expect("fixture should decode")
}

fn decoded_part_snapshot(parts: &[nav_harness::models::DecodedPart]) -> serde_json::Value {
    serde_json::Value::Array(
        parts
            .iter()
            .map(|part| {
                serde_json::json!({
                    "pointer": part.provider_json_pointer,
                    "part": part.part,
                })
            })
            .collect(),
    )
}

#[test]
fn assistant_text_decodes_with_provenance_and_turn_meta() {
    let decoded = decode(
        r#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":"hello there"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::Decoded);
    assert_eq!(decoded.turns.len(), 1);

    let turn = &decoded.turns[0].turn;
    assert_eq!(turn.run_id, run_id());
    assert_eq!(turn.seq, 0);
    assert_eq!(turn.role, TurnRole::Assistant);
    assert_eq!(turn.meta.model_provider.as_deref(), Some("openai"));
    assert_eq!(turn.meta.model_id.as_deref(), Some("gpt-5.1"));
    assert_eq!(turn.meta.api_kind, Some(ApiKind::OpenAiCompletions));
    assert_eq!(turn.meta.finish_reason.as_deref(), Some("stop"));
    assert_eq!(
        turn.meta.usage,
        Some(TokenUsage {
            input: 7,
            output: 3,
            reasoning: 0,
            cache_read: 0,
            cache_write: 0,
        })
    );

    let part = &decoded.turns[0].parts[0];
    assert_eq!(part.provider_payload_id, provider_payload_id());
    assert_eq!(part.provider_json_pointer, "/choices/0/message/content");
    assert_eq!(
        part.part,
        Part::Text {
            text: "hello there".to_string(),
            synthetic: None,
        }
    );
}

#[test]
fn tool_call_decodes_to_canonical_part_with_arguments() {
    let decoded = decode(
        r#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_read_1","type":"function","function":{"name":"read","arguments":"{\"path\":\"Cargo.toml\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12,"completion_tokens":5,"total_tokens":17}}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::Decoded);
    let part = &decoded.turns[0].parts[0];
    assert_eq!(part.provider_payload_id, provider_payload_id());
    assert_eq!(
        part.provider_json_pointer,
        "/choices/0/message/tool_calls/0"
    );

    let Part::ToolCall {
        id,
        name,
        arguments,
        raw_arguments_artifact_id,
    } = &part.part
    else {
        panic!("expected tool call part, got {:?}", part.part);
    };

    ToolCallId::try_new(id.as_str()).expect("decoded tool call id should be UUIDv7-shaped");
    assert_eq!(name, "read");
    assert_eq!(arguments, &serde_json::json!({"path": "Cargo.toml"}));
    assert_eq!(raw_arguments_artifact_id, &None);
}

#[test]
fn unknown_message_field_decodes_to_provider_opaque_and_marks_unknowns() {
    let decoded = decode(
        r#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":"hello","vendor_extra":{"nested":[true,false]}},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::DecodedWithUnknowns);
    assert_eq!(decoded.turns[0].parts.len(), 2);

    let opaque = &decoded.turns[0].parts[1];
    assert_eq!(opaque.provider_payload_id, provider_payload_id());
    assert_eq!(
        opaque.provider_json_pointer,
        "/choices/0/message/vendor_extra"
    );
    assert_eq!(
        opaque.part,
        Part::ProviderOpaque {
            api_kind: ApiKind::OpenAiCompletions,
            kind: "message.vendor_extra".to_string(),
            raw_artifact_id: artifact_id(),
            raw_payload: Some(
                nav_harness::sessions::RawJson::from_string(
                    r#"{"nested":[true,false]}"#.to_string()
                )
                .expect("raw JSON should parse"),
            ),
        }
    );
}

#[test]
fn unknown_message_field_preserves_raw_payload_bytes() {
    let decoded = decode(
        r#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":"hello","vendor_extra":{  "b" : 2, "a" : [ true ] }},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#,
    );

    let opaque = &decoded.turns[0].parts[1].part;
    let Part::ProviderOpaque {
        raw_payload: Some(raw_payload),
        ..
    } = opaque
    else {
        panic!("expected provider opaque raw payload, got {opaque:?}");
    };

    assert_eq!(raw_payload.get(), r#"{  "b" : 2, "a" : [ true ] }"#);
}

#[test]
fn unknown_message_field_pointer_escapes_json_pointer_tokens() {
    let decoded = decode(
        r#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":"hello","vendor/extra~meta":true},"finish_reason":"stop"}]}"#,
    );

    assert_eq!(
        decoded.turns[0].parts[1].provider_json_pointer,
        "/choices/0/message/vendor~1extra~0meta"
    );
}

#[test]
fn usage_details_preserve_reasoning_and_cached_tokens() {
    let decoded = decode(
        r#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":8,"total_tokens":28,"prompt_tokens_details":{"cached_tokens":5},"completion_tokens_details":{"reasoning_tokens":2}}}"#,
    );

    assert_eq!(
        decoded.turns[0].turn.meta.usage,
        Some(TokenUsage {
            input: 20,
            output: 8,
            reasoning: 2,
            cache_read: 5,
            cache_write: 0,
        })
    );
}

#[test]
fn unknown_tool_call_item_decodes_to_provider_opaque() {
    let decoded = decode(
        r#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_read_1","type":"function","function":{"name":"read","arguments":"{\"path\":\"Cargo.toml\"}"}},{"id":"custom_1","payload":{"x":1},"type":"custom_tool"}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12,"completion_tokens":5,"total_tokens":17}}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::DecodedWithUnknowns);
    assert_eq!(decoded.turns[0].parts.len(), 2);

    let opaque = &decoded.turns[0].parts[1];
    assert_eq!(
        opaque.provider_json_pointer,
        "/choices/0/message/tool_calls/1"
    );
    assert_eq!(
        opaque.part,
        Part::ProviderOpaque {
            api_kind: ApiKind::OpenAiCompletions,
            kind: "tool_call.custom_tool".to_string(),
            raw_artifact_id: artifact_id(),
            raw_payload: Some(
                nav_harness::sessions::RawJson::from_string(
                    r#"{"id":"custom_1","payload":{"x":1},"type":"custom_tool"}"#.to_string()
                )
                .expect("raw JSON should parse"),
            ),
        }
    );
}

#[test]
fn unknown_tool_call_item_preserves_raw_payload_bytes() {
    let decoded = decode(
        r#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_read_1","type":"function","function":{"name":"read","arguments":"{\"path\":\"Cargo.toml\"}"}},{  "type" : "custom_tool", "payload" : { "x" : 1 } }]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12,"completion_tokens":5,"total_tokens":17}}"#,
    );

    let opaque = &decoded.turns[0].parts[1].part;
    let Part::ProviderOpaque {
        raw_payload: Some(raw_payload),
        ..
    } = opaque
    else {
        panic!("expected provider opaque raw payload, got {opaque:?}");
    };

    assert_eq!(
        raw_payload.get(),
        r#"{  "type" : "custom_tool", "payload" : { "x" : 1 } }"#
    );
}

#[test]
fn non_object_message_is_malformed() {
    let err = decode_err(
        r#"{"id":"chatcmpl_1","model":"gpt-5.1","choices":[{"index":0,"message":"assistant said hi","finish_reason":"stop"}]}"#,
    );

    assert!(
        err.to_string()
            .contains("message for choice 0 is not an object"),
        "unexpected error: {err}"
    );
}

#[test]
fn responses_message_output_decodes_step_text_and_usage() {
    let decoded = decode_responses(
        r#"{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.1","output":[{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Hello from Responses.","annotations":[]}]}],"usage":{"input_tokens":11,"output_tokens":7,"total_tokens":18,"input_tokens_details":{"cached_tokens":4},"output_tokens_details":{"reasoning_tokens":2}}}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::Decoded);
    assert_eq!(decoded.turns.len(), 1);

    let turn = &decoded.turns[0].turn;
    assert_eq!(turn.run_id, run_id());
    assert_eq!(turn.seq, 0);
    assert_eq!(turn.role, TurnRole::Assistant);
    assert_eq!(turn.meta.model_provider.as_deref(), Some("openai"));
    assert_eq!(turn.meta.model_id.as_deref(), Some("gpt-5.1"));
    assert_eq!(turn.meta.api_kind, Some(ApiKind::OpenAiResponses));
    assert_eq!(turn.meta.finish_reason.as_deref(), Some("completed"));
    assert_eq!(
        turn.meta.usage,
        Some(TokenUsage {
            input: 11,
            output: 7,
            reasoning: 2,
            cache_read: 4,
            cache_write: 0,
        })
    );

    let parts = &decoded.turns[0].parts;
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].part, Part::StepStart { snapshot: None });
    assert_eq!(
        parts[1].part,
        Part::Text {
            text: "Hello from Responses.".to_string(),
            synthetic: None,
        }
    );
    assert_eq!(parts[1].provider_json_pointer, "/output/0/content/0/text");
    assert_eq!(
        parts[2].part,
        Part::StepFinish {
            reason: "completed".to_string(),
            cost: 0.0,
            tokens: TokenUsage {
                input: 11,
                output: 7,
                reasoning: 2,
                cache_read: 4,
                cache_write: 0,
            },
            snapshot: None,
        }
    );
}

#[test]
fn responses_encrypted_reasoning_decodes_to_thinking_part() {
    let decoded = decode_responses(
        r#"{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.1","output":[{"id":"rs_1","type":"reasoning","status":"completed","summary":[],"content":[],"encrypted_content":"enc_reasoning_payload"},{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Done.","annotations":[]}]}]}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::Decoded);
    let parts = &decoded.turns[0].parts;
    assert_eq!(parts.len(), 6);
    assert_eq!(parts[0].part, Part::StepStart { snapshot: None });
    assert_eq!(
        parts[1].part,
        Part::Thinking {
            text: "enc_reasoning_payload".to_string(),
            provider_hint: Some("encrypted".to_string()),
        }
    );
    assert_eq!(
        parts[1].provider_json_pointer,
        "/output/0/encrypted_content"
    );
    assert_eq!(
        parts[2].part,
        Part::StepFinish {
            reason: "completed".to_string(),
            cost: 0.0,
            tokens: TokenUsage::default(),
            snapshot: None,
        }
    );
}

#[test]
fn responses_reasoning_text_decodes_to_thinking_part() {
    let decoded = decode_responses(
        r#"{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.1","output":[{"id":"rs_1","type":"reasoning","status":"completed","summary":[{"type":"summary_text","text":"Checked the inputs."}],"content":[{"type":"reasoning_text","text":"Detailed reasoning."}]},{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Done.","annotations":[]}]}]}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::Decoded);
    let parts = &decoded.turns[0].parts;
    assert_eq!(parts.len(), 7);
    assert_eq!(
        parts[1].part,
        Part::Thinking {
            text: "Detailed reasoning.".to_string(),
            provider_hint: Some("reasoning_text".to_string()),
        }
    );
    assert_eq!(parts[1].provider_json_pointer, "/output/0/content/0/text");
    assert_eq!(
        parts[2].part,
        Part::Thinking {
            text: "Checked the inputs.".to_string(),
            provider_hint: Some("summary_text".to_string()),
        }
    );
    assert_eq!(parts[2].provider_json_pointer, "/output/0/summary/0/text");
}

#[test]
fn responses_tool_call_and_tool_result_round_decodes_and_reencodes() {
    let decoded = decode_responses(
        r#"{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.1","output":[{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_read_1","name":"read","arguments":"{\"path\":\"Cargo.toml\"}"},{"id":"fco_1","type":"function_call_output","status":"completed","call_id":"call_read_1","output":"1: [package]\n2: name = \"nav\""},{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Here is the file.","annotations":[]}]}]}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::Decoded);
    let parts = &decoded.turns[0].parts;
    assert_eq!(parts.len(), 9);

    let Part::ToolCall {
        id: tool_call_id,
        name,
        arguments,
        raw_arguments_artifact_id,
    } = &parts[1].part
    else {
        panic!("expected tool call part, got {:?}", parts[1].part);
    };
    assert_eq!(name, "read");
    assert_eq!(arguments, &serde_json::json!({"path": "Cargo.toml"}));
    assert_eq!(raw_arguments_artifact_id, &None);

    let Part::ToolResult {
        call_id,
        content,
        raw_artifact_id,
        is_error,
    } = &parts[4].part
    else {
        panic!("expected tool result part, got {:?}", parts[4].part);
    };
    assert_eq!(call_id, tool_call_id);
    assert_eq!(content, "1: [package]\n2: name = \"nav\"");
    assert_eq!(raw_artifact_id, &None);
    assert!(!is_error);

    let turns = vec![(
        decoded.turns[0].turn.clone(),
        parts
            .iter()
            .map(|part| part.part.clone())
            .collect::<Vec<_>>(),
    )];
    let encoder = nav_harness::models::OpenAiChatCompletionsEncoder::new();
    let request = encoder.encode(&turns).expect("encode should accept parts");

    assert_eq!(request.messages.len(), 2);
    let tool_calls = request.messages[0]
        .tool_calls
        .as_ref()
        .expect("assistant message should carry tool calls");
    assert_eq!(tool_calls[0].id, tool_call_id.as_str());
    assert_eq!(
        request.messages[1].tool_call_id.as_deref(),
        Some(tool_call_id.as_str())
    );
}

#[test]
fn responses_tool_call_and_tool_result_round_matches_snapshot() {
    let decoded = decode_responses(
        r#"{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.1","output":[{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_read_1","name":"read","arguments":"{\"path\":\"Cargo.toml\"}"},{"id":"fco_1","type":"function_call_output","status":"completed","call_id":"call_read_1","output":"1: [package]\n2: name = \"nav\""},{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Here is the file.","annotations":[]}]}]}"#,
    );

    let snapshot = decoded_part_snapshot(&decoded.turns[0].parts);

    assert_eq!(
        serde_json::to_string_pretty(&snapshot).expect("snapshot should serialize"),
        r#"[
  {
    "part": {
      "type": "step_start"
    },
    "pointer": "/output/0"
  },
  {
    "part": {
      "arguments": {
        "path": "Cargo.toml"
      },
      "id": "019f2f6f-f178-7a72-9f28-dbf0c2dccc6e",
      "name": "read",
      "type": "tool_call"
    },
    "pointer": "/output/0"
  },
  {
    "part": {
      "cost": 0.0,
      "reason": "completed",
      "tokens": {
        "cache_read": 0,
        "cache_write": 0,
        "input": 0,
        "output": 0,
        "reasoning": 0
      },
      "type": "step_finish"
    },
    "pointer": "/output/0"
  },
  {
    "part": {
      "type": "step_start"
    },
    "pointer": "/output/1"
  },
  {
    "part": {
      "call_id": "019f2f6f-f178-7a72-9f28-dbf0c2dccc6e",
      "content": "1: [package]\n2: name = \"nav\"",
      "is_error": false,
      "type": "tool_result"
    },
    "pointer": "/output/1/output"
  },
  {
    "part": {
      "cost": 0.0,
      "reason": "completed",
      "tokens": {
        "cache_read": 0,
        "cache_write": 0,
        "input": 0,
        "output": 0,
        "reasoning": 0
      },
      "type": "step_finish"
    },
    "pointer": "/output/1"
  },
  {
    "part": {
      "type": "step_start"
    },
    "pointer": "/output/2"
  },
  {
    "part": {
      "text": "Here is the file.",
      "type": "text"
    },
    "pointer": "/output/2/content/0/text"
  },
  {
    "part": {
      "cost": 0.0,
      "reason": "completed",
      "tokens": {
        "cache_read": 0,
        "cache_write": 0,
        "input": 0,
        "output": 0,
        "reasoning": 0
      },
      "type": "step_finish"
    },
    "pointer": "/output/2"
  }
]"#
    );
}

#[test]
fn responses_unknown_output_item_decodes_to_provider_opaque_with_raw_payload() {
    let decoded = decode_responses(
        r#"{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.1","output":[{  "type" : "custom_item", "payload" : { "x" : [ true, false ] } }]}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::DecodedWithUnknowns);
    let parts = &decoded.turns[0].parts;
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[1].provider_payload_id, provider_payload_id());
    assert_eq!(parts[1].provider_json_pointer, "/output/0");
    assert_eq!(
        parts[1].part,
        Part::ProviderOpaque {
            api_kind: ApiKind::OpenAiResponses,
            kind: "response.output_item.custom_item".to_string(),
            raw_artifact_id: artifact_id(),
            raw_payload: Some(
                nav_harness::sessions::RawJson::from_string(
                    r#"{  "type" : "custom_item", "payload" : { "x" : [ true, false ] } }"#
                        .to_string(),
                )
                .expect("raw JSON should parse"),
            ),
        }
    );
}
