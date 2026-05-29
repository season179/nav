//! Fixture-driven tests for the ChatGPT/Codex subscription canonical encoder.

use nav_harness::models::{
    ApiKind, ChatGptSubscriptionContentPart, ChatGptSubscriptionEncoder, ChatGptSubscriptionItem,
    ChatGptSubscriptionMessageItem, ChatGptSubscriptionMessageRole,
    ChatGptSubscriptionToolCallItem, ChatGptSubscriptionToolResultItem, Encoder,
};
use nav_harness::sessions::{
    ImageSource, ModelTurn, Part, ProviderState, RawJson, ToolCall, Turn, TurnMeta, TurnRole,
};
use nav_types::{ArtifactId, MessageId, RunId, ToolCallId};

fn run_id() -> RunId {
    RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").expect("test run id")
}

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test message id should be UUIDv7-shaped")
}

fn turn(role: TurnRole, seq: u32, parts: Vec<Part>) -> (Turn, Vec<Part>) {
    (
        Turn {
            id: message_id(seq as u64),
            run_id: run_id(),
            seq,
            role,
            meta: TurnMeta::default(),
            created_at: 1_700_000_000_000 + i64::from(seq),
        },
        parts,
    )
}

#[test]
fn text_part_encodes_as_response_style_message_item_with_session_metadata() {
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::Text {
            text: "Hello from Codex.".to_string(),
            synthetic: None,
        }],
    )];

    let request = ChatGptSubscriptionEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(request.metadata.run_id.as_deref(), Some(run_id().as_str()));
    assert_eq!(request.metadata.turn_count, 1);
    assert_eq!(
        request.metadata.last_turn_id.as_deref(),
        Some(message_id(1).as_str())
    );
    assert_eq!(
        request.items,
        vec![ChatGptSubscriptionItem::Message(
            ChatGptSubscriptionMessageItem {
                id: Some(message_id(1).to_string()),
                role: ChatGptSubscriptionMessageRole::Assistant,
                content: vec![ChatGptSubscriptionContentPart::OutputText {
                    text: "Hello from Codex.".to_string(),
                }],
            }
        )]
    );
}

#[test]
fn image_part_encodes_as_response_style_image_content() {
    let artifact_id =
        ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").expect("test artifact id");
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

    let request = ChatGptSubscriptionEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.items,
        vec![ChatGptSubscriptionItem::Message(
            ChatGptSubscriptionMessageItem {
                id: Some(message_id(1).to_string()),
                role: ChatGptSubscriptionMessageRole::User,
                content: vec![ChatGptSubscriptionContentPart::InputImage {
                    image_url: format!("artifact://{artifact_id}"),
                }],
            }
        )]
    );
}

#[test]
fn tool_call_part_encodes_as_response_style_function_call_item() {
    let call_id =
        ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").expect("test tool call id");
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

    let request = ChatGptSubscriptionEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.items,
        vec![ChatGptSubscriptionItem::ToolCall(
            ChatGptSubscriptionToolCallItem {
                call_id: call_id.to_string(),
                name: "read".to_string(),
                arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
            }
        )]
    );
}

#[test]
fn tool_result_part_encodes_as_response_style_function_output_item() {
    let call_id =
        ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").expect("test tool call id");
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::ToolResult {
            call_id: call_id.clone(),
            content: "1: [package]\n2: name = \"nav\"".to_string(),
            raw_artifact_id: None,
            is_error: true,
        }],
    )];

    let request = ChatGptSubscriptionEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.items,
        vec![ChatGptSubscriptionItem::ToolResult(
            ChatGptSubscriptionToolResultItem {
                call_id: call_id.to_string(),
                output: "1: [package]\n2: name = \"nav\"".to_string(),
                is_error: true,
            }
        )]
    );
}

#[test]
fn matching_provider_state_adds_previous_response_id_to_request() {
    let provider_state = ProviderState {
        run_id: run_id(),
        api_kind: "chatgpt_subscription".to_string(),
        state_json: r#"{"previous_response_id":"resp_123"}"#.to_string(),
    };
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Text {
            text: "continue".to_string(),
            synthetic: None,
        }],
    )];

    let request = ChatGptSubscriptionEncoder::new()
        .with_provider_state(Some(&provider_state))
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(request.previous_response_id.as_deref(), Some("resp_123"));
}

#[test]
fn provider_state_for_another_run_is_not_chained() {
    let provider_state = ProviderState {
        run_id: RunId::try_new("019f2f6f-f178-7a72-9f28-000000000999").expect("test run id"),
        api_kind: "chatgpt_subscription".to_string(),
        state_json: r#"{"previous_response_id":"resp_wrong_run"}"#.to_string(),
    };
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Text {
            text: "continue".to_string(),
            synthetic: None,
        }],
    )];

    let request = ChatGptSubscriptionEncoder::new()
        .with_provider_state(Some(&provider_state))
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(request.previous_response_id, None);
}

#[test]
fn compaction_and_provider_opaque_parts_encode_as_synthetic_text() {
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![
            Part::Compaction {
                auto: true,
                tail_start_id: None,
            },
            Part::ProviderOpaque {
                api_kind: ApiKind::ChatGptSubscription,
                kind: "response.output_item.unknown".to_string(),
                raw_artifact_id: ArtifactId::try_new("art_0000018bcfe56800_0000000000000001")
                    .expect("test artifact id"),
                raw_payload: Some(
                    RawJson::from_string(r#"{"vendor":true}"#.to_string())
                        .expect("raw JSON should parse"),
                ),
            },
        ],
    )];

    let request = ChatGptSubscriptionEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.items,
        vec![ChatGptSubscriptionItem::Message(
            ChatGptSubscriptionMessageItem {
                id: Some(message_id(1).to_string()),
                role: ChatGptSubscriptionMessageRole::User,
                content: vec![
                    ChatGptSubscriptionContentPart::InputText {
                        text: "Context was compacted. Previous conversation history has been summarized."
                            .to_string(),
                    },
                    ChatGptSubscriptionContentPart::InputText {
                        text: "[Provider-specific content: opaque]".to_string(),
                    },
                ],
            }
        )]
    );
}

#[test]
fn api_kind_names_chatgpt_subscription_dialect() {
    assert_eq!(
        serde_json::to_value(ApiKind::ChatGptSubscription).expect("serialize api kind"),
        serde_json::json!("chatgpt-subscription")
    );
    assert_eq!(
        serde_json::from_str::<ApiKind>(r#""chatgpt_subscription""#)
            .expect("deserialize legacy api kind"),
        ApiKind::ChatGptSubscription
    );
}

#[test]
fn encoder_trait_maps_model_turn_tool_items() {
    let tool_call = ToolCall {
        id: "call_read_1".to_string(),
        tool_call_id: None,
        name: "read".to_string(),
        arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
    };
    let turns = vec![
        ModelTurn::assistant_text_with_tool_calls("Let me read that.", vec![tool_call]),
        ModelTurn::tool_result("call_read_1", "file contents"),
    ];

    let request = Encoder::encode(&ChatGptSubscriptionEncoder::new(), &turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.items,
        vec![
            ChatGptSubscriptionItem::Message(ChatGptSubscriptionMessageItem {
                id: None,
                role: ChatGptSubscriptionMessageRole::Assistant,
                content: vec![ChatGptSubscriptionContentPart::OutputText {
                    text: "Let me read that.".to_string(),
                }],
            }),
            ChatGptSubscriptionItem::ToolCall(ChatGptSubscriptionToolCallItem {
                call_id: "call_read_1".to_string(),
                name: "read".to_string(),
                arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
            }),
            ChatGptSubscriptionItem::ToolResult(ChatGptSubscriptionToolResultItem {
                call_id: "call_read_1".to_string(),
                output: "file contents".to_string(),
                is_error: false,
            }),
        ]
    );
}

#[test]
fn encoder_trait_preserves_system_turn_role() {
    let turns = vec![ModelTurn::system_text("you are nav")];

    let request = Encoder::encode(&ChatGptSubscriptionEncoder::new(), &turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.items,
        vec![ChatGptSubscriptionItem::Message(
            ChatGptSubscriptionMessageItem {
                id: None,
                role: ChatGptSubscriptionMessageRole::System,
                content: vec![ChatGptSubscriptionContentPart::InputText {
                    text: "you are nav".to_string(),
                }],
            }
        )]
    );
}

#[test]
fn request_serializes_to_response_style_wire_shape() {
    let call_id =
        ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").expect("test tool call id");
    let turns = vec![
        turn(
            TurnRole::User,
            1,
            vec![
                Part::Text {
                    text: "Read Cargo.toml".to_string(),
                    synthetic: None,
                },
                Part::Image {
                    mime: "image/png".to_string(),
                    source: ImageSource::InlineBytes {
                        bytes: vec![1, 2, 3],
                    },
                },
            ],
        ),
        turn(
            TurnRole::Assistant,
            2,
            vec![Part::ToolCall {
                id: call_id.clone(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
                raw_arguments_artifact_id: None,
            }],
        ),
        turn(
            TurnRole::Assistant,
            3,
            vec![Part::ToolResult {
                call_id: call_id.clone(),
                content: "file contents".to_string(),
                raw_artifact_id: None,
                is_error: false,
            }],
        ),
    ];

    let request = ChatGptSubscriptionEncoder::new()
        .with_previous_response_id("resp_123")
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        serde_json::to_value(&request).expect("request should serialize"),
        serde_json::json!({
            "items": [
                {
                    "type": "message",
                    "id": message_id(1).to_string(),
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": "Read Cargo.toml" },
                        { "type": "input_image", "image_url": "data:image/png;base64,<3 bytes>" }
                    ]
                },
                {
                    "type": "tool_call",
                    "call_id": call_id.to_string(),
                    "name": "read",
                    "arguments": "{\"path\":\"Cargo.toml\"}"
                },
                {
                    "type": "tool_result",
                    "call_id": call_id.to_string(),
                    "output": "file contents",
                    "is_error": false
                }
            ],
            "metadata": {
                "run_id": run_id().to_string(),
                "turn_count": 3,
                "last_turn_id": message_id(3).to_string()
            },
            "previous_response_id": "resp_123"
        })
    );
}
