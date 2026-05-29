//! Fixture-driven tests for the Anthropic Messages canonical encoder.

use nav_harness::models::{AnthropicMessagesEncoder, AnthropicToolDefinition, ApiKind, Encoder};
use nav_harness::sessions::{ImageSource, ModelTurn, Part, ToolCall, Turn, TurnMeta, TurnRole};
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

#[test]
fn api_kind_accepts_anthropic_messages_spellings() {
    let hyphenated: ApiKind = serde_json::from_str(r#""anthropic-messages""#).unwrap();
    let underscored: ApiKind = serde_json::from_str(r#""anthropic_messages""#).unwrap();

    assert_eq!(hyphenated, ApiKind::AnthropicMessages);
    assert_eq!(underscored, ApiKind::AnthropicMessages);
}

#[test]
fn encoder_trait_maps_system_turns_to_top_level_system() {
    let encoder = AnthropicMessagesEncoder::new();
    let turns = vec![
        ModelTurn::system_text("You are concise."),
        ModelTurn::user_text("Say hi."),
    ];

    let request = <AnthropicMessagesEncoder as Encoder>::encode(&encoder, &turns)
        .expect("encoding should succeed");

    assert_eq!(request.system.as_deref(), Some("You are concise."));
    assert_eq!(
        request.messages,
        vec![serde_json::json!({
            "role": "user",
            "content": [{
                "type": "text",
                "text": "Say hi."
            }]
        })]
    );
}

#[test]
fn encoder_trait_maps_tool_parts_to_anthropic_messages() {
    let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
    let encoder = AnthropicMessagesEncoder::new();
    let turns = vec![
        ModelTurn::assistant_text_with_tool_calls(
            "I'll read that.",
            vec![ToolCall {
                id: "internal-call-id".to_string(),
                tool_call_id: Some(call_id.clone()),
                name: "read".to_string(),
                arguments: "{\"path\":\"Cargo.toml\"}".to_string(),
            }],
        ),
        ModelTurn::tool_result(call_id.as_str(), "file contents"),
    ];

    let request = <AnthropicMessagesEncoder as Encoder>::encode(&encoder, &turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.messages,
        vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": "I'll read that."
                    },
                    {
                        "type": "tool_use",
                        "id": call_id.as_str(),
                        "name": "read",
                        "input": {"path": "Cargo.toml"}
                    }
                ]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": call_id.as_str(),
                    "content": "file contents"
                }]
            })
        ]
    );
}

#[test]
fn encoder_with_tools_includes_anthropic_tool_definitions() {
    let tools = vec![AnthropicToolDefinition {
        name: "read".to_string(),
        description: "Read a file".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        }),
    }];
    let turns = vec![ModelTurn::user_text("Read Cargo.toml")];

    let request = <AnthropicMessagesEncoder as Encoder>::encode(
        &AnthropicMessagesEncoder::new().with_tools(tools),
        &turns,
    )
    .expect("encoding should succeed");

    assert_eq!(request.tools.len(), 1);
    assert_eq!(request.tools[0].name, "read");
    assert_eq!(request.tools[0].description, "Read a file");
    assert_eq!(request.tools[0].input_schema["required"][0], "path");
}

#[test]
fn canonical_user_image_part_produces_anthropic_image_block() {
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

    let request = AnthropicMessagesEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.messages,
        vec![serde_json::json!({
            "role": "user",
            "content": [{
                "type": "image",
                "source": {
                    "type": "url",
                    "url": format!("artifact://{}", artifact_id.as_str())
                }
            }]
        })]
    );
}

#[test]
fn canonical_inline_image_part_encodes_base64_data() {
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Image {
            mime: "image/png".to_string(),
            source: ImageSource::InlineBytes {
                bytes: b"hi".to_vec(),
            },
        }],
    )];

    let request = AnthropicMessagesEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.messages[0]["content"][0]["source"]["type"],
        "base64"
    );
    assert_eq!(
        request.messages[0]["content"][0]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(request.messages[0]["content"][0]["source"]["data"], "aGk=");
}

#[test]
fn canonical_tool_use_and_tool_result_preserve_call_ids() {
    let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
    let turns = vec![
        turn(
            TurnRole::Assistant,
            1,
            vec![Part::ToolCall {
                id: call_id.clone(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
                raw_arguments_artifact_id: None,
            }],
        ),
        turn(
            TurnRole::Assistant,
            2,
            vec![Part::ToolResult {
                call_id: call_id.clone(),
                content: "1: [package]\n2: name = \"nav\"".to_string(),
                raw_artifact_id: None,
                is_error: false,
            }],
        ),
    ];

    let request = AnthropicMessagesEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.messages,
        vec![
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": call_id.as_str(),
                    "name": "read",
                    "input": {"path": "Cargo.toml"}
                }]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": call_id.as_str(),
                    "content": "1: [package]\n2: name = \"nav\""
                }]
            })
        ]
    );
}

#[test]
fn canonical_tool_result_splits_following_assistant_text() {
    let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![
            Part::ToolCall {
                id: call_id.clone(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
                raw_arguments_artifact_id: None,
            },
            Part::ToolResult {
                call_id: call_id.clone(),
                content: "file contents".to_string(),
                raw_artifact_id: None,
                is_error: false,
            },
            Part::Text {
                text: "Here is the file.".to_string(),
                synthetic: None,
            },
        ],
    )];

    let request = AnthropicMessagesEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.messages,
        vec![
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": call_id.as_str(),
                    "name": "read",
                    "input": {"path": "Cargo.toml"}
                }]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": call_id.as_str(),
                    "content": "file contents"
                }]
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "text",
                    "text": "Here is the file."
                }]
            })
        ]
    );
}

#[test]
fn canonical_thinking_part_produces_thinking_content_block() {
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::Thinking {
            text: "I should inspect the manifest first.".to_string(),
            provider_hint: Some("anthropic".to_string()),
        }],
    )];

    let request = AnthropicMessagesEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    assert_eq!(
        request.messages,
        vec![serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "thinking",
                "thinking": "I should inspect the manifest first."
            }]
        })]
    );
}
