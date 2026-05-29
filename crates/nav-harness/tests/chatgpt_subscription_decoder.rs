//! Fixture-driven tests for the ChatGPT/Codex subscription canonical decoder.

use nav_harness::models::{ChatGptSubscriptionDecodeInput, ChatGptSubscriptionDecoder, Decoder};
use nav_harness::sessions::{DecodeStatus, Part, TokenUsage, TurnRole};
use nav_types::{ArtifactId, ProviderPayloadId, RunId, ToolCallId};

fn provider_payload_id() -> ProviderPayloadId {
    ProviderPayloadId::try_new("pay_0000018bcfe56800_0000000000000048")
        .expect("test provider payload id")
}

fn artifact_id() -> ArtifactId {
    ArtifactId::try_new("art_0000018bcfe56800_0000000000000048").expect("test artifact id")
}

fn run_id() -> RunId {
    RunId::try_new("019f2f6f-f178-7a72-9f28-000000000048").expect("test run id")
}

fn decode(raw_json: &str) -> nav_harness::models::DecodedProviderPayload {
    let decoder = ChatGptSubscriptionDecoder::new();
    decoder
        .decode(&ChatGptSubscriptionDecodeInput {
            provider_payload_id: provider_payload_id(),
            raw_artifact_id: artifact_id(),
            run_id: run_id(),
            provider_id: Some("chatgpt".to_string()),
            raw_json: raw_json.as_bytes().to_vec(),
            created_at: 1_700_000_000_000,
        })
        .expect("fixture should decode")
}

#[test]
fn multi_event_stream_decodes_to_canonical_parts_in_event_order() {
    let decoded = decode(
        r#"{"events":[{"type":"response.created","response":{"id":"resp_sub_1","model":"gpt-5.1-codex"}},{"type":"response.output_item.added","output_index":0,"item":{"id":"rs_1","type":"reasoning","encrypted_content":"enc_reasoning_1"}},{"type":"response.output_item.done","output_index":0,"item":{"id":"rs_1","type":"reasoning","encrypted_content":"enc_reasoning_1"}},{"type":"response.output_item.added","output_index":1,"item":{"id":"msg_1","type":"message","role":"assistant","content":[]}},{"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"hello "},{"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Season"},{"type":"response.output_text.done","output_index":1,"content_index":0,"text":"hello Season"},{"type":"response.output_item.added","output_index":2,"item":{"id":"fc_1","type":"function_call","call_id":"call_read_1","name":"read","arguments":""}},{"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\""},{"type":"response.function_call_arguments.delta","output_index":2,"delta":":\"Cargo.toml\"}"},{"type":"response.output_item.done","output_index":2,"item":{"id":"fc_1","type":"function_call","call_id":"call_read_1","name":"read","arguments":"{\"path\":\"Cargo.toml\"}"}},{"type":"response.completed","response":{"id":"resp_sub_1","model":"gpt-5.1-codex","status":"completed","usage":{"input_tokens":11,"output_tokens":7,"output_tokens_details":{"reasoning_tokens":2}}}}]}"#,
    );

    assert_eq!(decoded.status, DecodeStatus::Decoded);
    assert_eq!(decoded.turns.len(), 1);

    let turn = &decoded.turns[0].turn;
    assert_eq!(turn.run_id, run_id());
    assert_eq!(turn.seq, 0);
    assert_eq!(turn.role, TurnRole::Assistant);
    assert_eq!(turn.meta.model_provider.as_deref(), Some("chatgpt"));
    assert_eq!(turn.meta.model_id.as_deref(), Some("gpt-5.1-codex"));
    assert_eq!(turn.meta.finish_reason.as_deref(), Some("completed"));
    assert_eq!(
        turn.meta.usage,
        Some(TokenUsage {
            input: 11,
            output: 7,
            reasoning: 2,
            cache_read: 0,
            cache_write: 0,
        })
    );

    let parts = &decoded.turns[0].parts;
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].provider_payload_id, provider_payload_id());
    assert_eq!(
        parts[0].provider_json_pointer,
        "/events/2/item/encrypted_content"
    );
    assert_eq!(
        parts[0].part,
        Part::Thinking {
            text: "enc_reasoning_1".to_string(),
            provider_hint: Some("encrypted".to_string()),
        }
    );

    assert_eq!(parts[1].provider_json_pointer, "/events/6/text");
    assert_eq!(
        parts[1].part,
        Part::Text {
            text: "hello Season".to_string(),
            synthetic: None,
        }
    );

    assert_eq!(parts[2].provider_json_pointer, "/events/10/item");
    let Part::ToolCall {
        id,
        name,
        arguments,
        raw_arguments_artifact_id,
    } = &parts[2].part
    else {
        panic!("expected tool call part, got {:?}", parts[2].part);
    };

    ToolCallId::try_new(id.as_str()).expect("decoded tool call id should be UUIDv7-shaped");
    assert_eq!(name, "read");
    assert_eq!(arguments, &serde_json::json!({"path": "Cargo.toml"}));
    assert_eq!(raw_arguments_artifact_id, &None);
}

#[test]
fn completed_message_item_does_not_duplicate_completed_text_event() {
    let decoded = decode(
        r#"{"events":[{"type":"response.created","response":{"id":"resp_sub_1","model":"gpt-5.1-codex"}},{"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"hello"},{"type":"response.output_text.done","output_index":0,"content_index":0,"text":"hello"},{"type":"response.output_item.done","output_index":0,"item":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}},{"type":"response.completed","response":{"id":"resp_sub_1","model":"gpt-5.1-codex","status":"completed"}}]}"#,
    );

    let text_parts = decoded.turns[0]
        .parts
        .iter()
        .filter(|part| matches!(part.part, Part::Text { .. }))
        .collect::<Vec<_>>();

    assert_eq!(text_parts.len(), 1);
    assert_eq!(
        text_parts[0].part,
        Part::Text {
            text: "hello".to_string(),
            synthetic: None,
        }
    );
    assert_eq!(text_parts[0].provider_json_pointer, "/events/2/text");
}

#[test]
fn final_response_metadata_overrides_created_metadata() {
    let decoded = decode(
        r#"{"events":[{"type":"response.created","response":{"id":"resp_sub_1","model":"initial-model","status":"in_progress","usage":{"input_tokens":1,"output_tokens":0}}},{"type":"response.output_text.done","output_index":0,"content_index":0,"text":"done"},{"type":"response.completed","response":{"id":"resp_sub_1","model":"gpt-5.1-codex","status":"completed","usage":{"input_tokens":11,"output_tokens":7,"output_tokens_details":{"reasoning_tokens":2}}}}]}"#,
    );

    let turn = &decoded.turns[0].turn;
    assert_eq!(turn.meta.model_id.as_deref(), Some("gpt-5.1-codex"));
    assert_eq!(turn.meta.finish_reason.as_deref(), Some("completed"));
    assert_eq!(
        turn.meta.usage,
        Some(TokenUsage {
            input: 11,
            output: 7,
            reasoning: 2,
            cache_read: 0,
            cache_write: 0,
        })
    );
}

#[test]
fn function_call_uses_coalesced_argument_deltas_when_done_item_is_empty() {
    let decoded = decode(
        r#"{"events":[{"type":"response.created","response":{"id":"resp_sub_1","model":"gpt-5.1-codex"}},{"type":"response.output_item.added","output_index":0,"item":{"id":"fc_1","type":"function_call","name":"read","arguments":""}},{"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"path\""},{"type":"response.function_call_arguments.delta","output_index":0,"delta":":\"Cargo.toml\"}"},{"type":"response.function_call_arguments.done","output_index":0,"arguments":""},{"type":"response.output_item.done","output_index":0,"item":{"id":"fc_1","type":"function_call","name":"read","arguments":""}},{"type":"response.completed","response":{"id":"resp_sub_1","model":"gpt-5.1-codex","status":"completed"}}]}"#,
    );

    let Part::ToolCall { arguments, .. } = &decoded.turns[0].parts[0].part else {
        panic!(
            "expected tool call part, got {:?}",
            decoded.turns[0].parts[0].part
        );
    };

    assert_eq!(arguments, &serde_json::json!({"path": "Cargo.toml"}));
}

#[test]
fn empty_text_done_event_uses_buffered_deltas() {
    let decoded = decode(
        r#"{"events":[{"type":"response.created","response":{"id":"resp_sub_1","model":"gpt-5.1-codex"}},{"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"hello"},{"type":"response.output_text.done","output_index":0,"content_index":0,"text":""},{"type":"response.completed","response":{"id":"resp_sub_1","model":"gpt-5.1-codex","status":"completed"}}]}"#,
    );

    assert_eq!(
        decoded.turns[0].parts[0].part,
        Part::Text {
            text: "hello".to_string(),
            synthetic: None,
        }
    );
    assert_eq!(
        decoded.turns[0].parts[0].provider_json_pointer,
        "/events/2/text"
    );
}
