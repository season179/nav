//! Fixture-driven tests for the OpenAI Responses canonical encoder.

use nav_harness::models::{ApiKind, Encoder, OpenAiResponsesEncoder, OpenAiResponsesRequest};
use nav_harness::sessions::{
    ImageSource, ModelTurn, Part, ProviderState, RawJson, ToolCall, Turn, TurnMeta, TurnRole,
};
use nav_types::{ArtifactId, MessageId, RunId, ToolCallId};

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test message id should be UUIDv7-shaped")
}

fn run_id(suffix: u64) -> RunId {
    RunId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test run id should be UUIDv7-shaped")
}

fn turn(role: TurnRole, seq: u32, parts: Vec<Part>) -> (Turn, Vec<Part>) {
    (
        Turn {
            id: message_id(seq as u64),
            run_id: run_id(1),
            seq,
            role,
            meta: TurnMeta::default(),
            created_at: 1_700_000_000_000,
        },
        parts,
    )
}

fn provider_state(api_kind: &str, previous_response_id: &str) -> ProviderState {
    ProviderState {
        run_id: run_id(1),
        api_kind: api_kind.to_string(),
        state_json: format!(r#"{{"previous_response_id":"{previous_response_id}"}}"#),
    }
}

#[test]
fn api_kind_accepts_openai_responses_spellings() {
    let hyphenated: ApiKind = serde_json::from_str(r#""openai-responses""#).unwrap();
    let underscored: ApiKind = serde_json::from_str(r#""openai_responses""#).unwrap();

    assert_eq!(hyphenated, ApiKind::OpenAiResponses);
    assert_eq!(underscored, ApiKind::OpenAiResponses);
}

fn encode(turns: &[(Turn, Vec<Part>)]) -> OpenAiResponsesRequest {
    OpenAiResponsesEncoder::new()
        .encode(turns)
        .expect("encoding should succeed")
}

#[test]
fn user_text_part_produces_responses_input_message() {
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Text {
            text: "What is Rust?".to_string(),
            synthetic: None,
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.input.len(), 1);
    assert_eq!(
        request.input[0],
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "What is Rust?"
            }]
        })
    );
}

#[test]
fn user_image_part_produces_responses_input_image_message() {
    let artifact_id = ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").unwrap();
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Image {
            mime: "image/png".to_string(),
            source: ImageSource::FileRef {
                artifact_id: artifact_id.clone(),
            },
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.input.len(), 1);
    assert_eq!(
        request.input[0],
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_image",
                "image_url": format!("artifact://{}", artifact_id.as_str())
            }]
        })
    );
}

#[test]
fn user_message_preserves_text_and_image_part_order() {
    let artifact_id = ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").unwrap();
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![
            Part::Image {
                mime: "image/png".to_string(),
                source: ImageSource::FileRef {
                    artifact_id: artifact_id.clone(),
                },
            },
            Part::Text {
                text: "What is in this image?".to_string(),
                synthetic: None,
            },
        ],
    )];

    let request = encode(&turns);

    assert_eq!(
        request.input[0]["content"],
        serde_json::json!([
            {
                "type": "input_image",
                "image_url": format!("artifact://{}", artifact_id.as_str())
            },
            {
                "type": "input_text",
                "text": "What is in this image?"
            }
        ])
    );
}

#[test]
fn assistant_text_part_produces_completed_output_text_message() {
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::Text {
            text: "Rust is a systems programming language.".to_string(),
            synthetic: None,
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.input.len(), 1);
    assert_eq!(
        request.input[0],
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": "Rust is a systems programming language.",
                "annotations": []
            }]
        })
    );
}

#[test]
fn tool_call_part_produces_responses_function_call_item() {
    let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::ToolCall {
            id: call_id.clone(),
            name: "read".to_string(),
            arguments: serde_json::json!({"path": "Cargo.toml"}),
            raw_arguments_artifact_id: None,
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.input.len(), 1);
    assert_eq!(
        request.input[0],
        serde_json::json!({
            "type": "function_call",
            "call_id": call_id.as_str(),
            "name": "read",
            "arguments": "{\"path\":\"Cargo.toml\"}",
            "status": "completed"
        })
    );
}

#[test]
fn tool_result_part_produces_responses_function_call_output_item() {
    let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::ToolResult {
            call_id: call_id.clone(),
            content: "1: [package]\n2: name = \"nav\"".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.input.len(), 1);
    assert_eq!(
        request.input[0],
        serde_json::json!({
            "type": "function_call_output",
            "call_id": call_id.as_str(),
            "output": "1: [package]\n2: name = \"nav\""
        })
    );
}

#[test]
fn matching_provider_state_attaches_previous_response_id() {
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Text {
            text: "continue".to_string(),
            synthetic: None,
        }],
    )];
    let state = provider_state("openai_responses", "resp_123");

    let request = OpenAiResponsesEncoder::new()
        .with_provider_state(Some(state))
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(request.previous_response_id.as_deref(), Some("resp_123"));
}

#[test]
fn provider_state_for_different_api_kind_drops_previous_response_id() {
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Text {
            text: "continue".to_string(),
            synthetic: None,
        }],
    )];
    let state = provider_state("openai_chat_completions", "resp_wrong_api");

    let request = OpenAiResponsesEncoder::new()
        .with_provider_state(Some(state))
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(request.previous_response_id, None);
}

#[test]
fn encrypted_thinking_part_produces_reasoning_item() {
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::Thinking {
            text: "gAAAAABm-encrypted-reasoning".to_string(),
            provider_hint: Some("encrypted".to_string()),
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.input.len(), 1);
    assert_eq!(
        request.input[0],
        serde_json::json!({
            "type": "reasoning",
            "encrypted_content": "gAAAAABm-encrypted-reasoning",
            "summary": []
        })
    );
}

#[test]
fn provider_opaque_part_produces_synthetic_output_text_message() {
    let raw_artifact_id = ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").unwrap();
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::ProviderOpaque {
            api_kind: ApiKind::OpenAiResponses,
            kind: "response.output_item.unknown".to_string(),
            raw_artifact_id,
            raw_payload: Some(RawJson::from_string(r#"{"unknown":true}"#.to_string()).unwrap()),
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.input.len(), 1);
    assert_eq!(request.input[0]["role"], "assistant");
    assert_eq!(request.input[0]["content"][0]["type"], "output_text");
    assert!(
        request.input[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("opaque")
    );
}

#[test]
fn assistant_items_preserve_canonical_part_order() {
    let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![
            Part::Thinking {
                text: "encrypted-reasoning".to_string(),
                provider_hint: Some("encrypted".to_string()),
            },
            Part::ToolCall {
                id: call_id,
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
                raw_arguments_artifact_id: None,
            },
            Part::Text {
                text: "Done.".to_string(),
                synthetic: None,
            },
        ],
    )];

    let request = encode(&turns);

    assert_eq!(
        request
            .input
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["reasoning", "function_call", "message"]
    );
    assert_eq!(request.input[2]["content"][0]["text"], "Done.");
}

#[test]
fn encoder_trait_maps_system_turns_to_instructions() {
    let encoder = OpenAiResponsesEncoder::new();
    let turns = vec![
        ModelTurn::system_text("You are concise."),
        ModelTurn::user_text("Say hi."),
    ];

    let request = <OpenAiResponsesEncoder as Encoder>::encode(&encoder, &turns)
        .expect("encoding should succeed");

    assert_eq!(request.instructions.as_deref(), Some("You are concise."));
    assert_eq!(
        request.input,
        vec![serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": "Say hi."
            }]
        })]
    );
}

#[test]
fn encoder_trait_maps_tool_parts_to_responses_items() {
    let encoder = OpenAiResponsesEncoder::new();
    let turns = vec![
        ModelTurn::assistant_tool_calls(vec![ToolCall {
            id: "call_read_1".to_string(),
            tool_call_id: None,
            name: "read".to_string(),
            arguments: "{\"path\":\"Cargo.toml\"}".to_string(),
        }]),
        ModelTurn::tool_result("call_read_1", "file contents"),
    ];

    let request = <OpenAiResponsesEncoder as Encoder>::encode(&encoder, &turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.input,
        vec![
            serde_json::json!({
                "type": "function_call",
                "call_id": "call_read_1",
                "name": "read",
                "arguments": "{\"path\":\"Cargo.toml\"}",
                "status": "completed"
            }),
            serde_json::json!({
                "type": "function_call_output",
                "call_id": "call_read_1",
                "output": "file contents"
            }),
        ]
    );
}
