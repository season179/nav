//! Fixture-driven tests for the OpenAI Chat Completions canonical decoder.

use nav_harness::models::{
    ApiKind, Decoder, OpenAiChatCompletionsDecodeInput, OpenAiChatCompletionsDecoder,
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
