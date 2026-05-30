//! Fixture-driven tests for the OpenAI Chat Completions canonical encoder.
//!
//! Each test covers one canonical-Part → wire-format mapping.

use nav_harness::models::ApiKind;
use nav_harness::models::{
    ChatCompletionMessageRole, ChatCompletionToolDefinition, OpenAiChatCompletionsEncoder,
    OpenAiCompletionsRequest,
};
use nav_harness::sessions::{ImageSource, Part, RawJson, Turn, TurnMeta, TurnRole};
use nav_types::{ArtifactId, MessageId, ToolCallId};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test message id should be UUIDv7-shaped")
}

fn turn(role: TurnRole, seq: u32, parts: Vec<Part>) -> (Turn, Vec<Part>) {
    (
        Turn {
            id: message_id(seq as u64),
            run_id: nav_types::RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001")
                .expect("test run id"),
            seq,
            role,
            meta: TurnMeta::default(),
            created_at: 1_700_000_000_000,
        },
        parts,
    )
}

fn encode(turns: &[(Turn, Vec<Part>)]) -> OpenAiCompletionsRequest {
    let encoder = OpenAiChatCompletionsEncoder::new();
    encoder.encode(turns).expect("encoding should succeed")
}

// ---------------------------------------------------------------------------
// Slice 1: Text part
// ---------------------------------------------------------------------------

#[test]
fn text_part_on_assistant_turn_produces_assistant_message_with_content() {
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::Text {
            text: "Hello, world!".to_string(),
            synthetic: None,
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.messages.len(), 1);
    assert_eq!(
        request.messages[0].role,
        ChatCompletionMessageRole::Assistant
    );
    assert_eq!(
        request.messages[0].content,
        Some(serde_json::json!("Hello, world!"))
    );
}

#[test]
fn text_part_on_user_turn_produces_user_message() {
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Text {
            text: "What is Rust?".to_string(),
            synthetic: None,
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.messages.len(), 1);
    assert_eq!(request.messages[0].role, ChatCompletionMessageRole::User);
    assert_eq!(
        request.messages[0].content,
        Some(serde_json::json!("What is Rust?"))
    );
}

// ---------------------------------------------------------------------------
// Slice 2: ToolCall part
// ---------------------------------------------------------------------------

#[test]
fn tool_call_part_produces_assistant_message_with_tool_calls() {
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

    assert_eq!(request.messages.len(), 1);
    assert_eq!(
        request.messages[0].role,
        ChatCompletionMessageRole::Assistant
    );
    let tool_calls = request.messages[0]
        .tool_calls
        .as_ref()
        .expect("should have tool_calls");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, call_id.as_str());
    assert_eq!(tool_calls[0].function.name, "read");
    assert_eq!(tool_calls[0].function.arguments, r#"{"path":"Cargo.toml"}"#);
}

// ---------------------------------------------------------------------------
// Slice 3: ToolResult part
// ---------------------------------------------------------------------------

#[test]
fn tool_result_part_produces_tool_role_message() {
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

    assert_eq!(request.messages.len(), 1);
    assert_eq!(request.messages[0].role, ChatCompletionMessageRole::Tool);
    assert_eq!(
        request.messages[0].tool_call_id.as_deref(),
        Some(call_id.as_str())
    );
    assert_eq!(
        request.messages[0].content,
        Some(serde_json::json!("1: [package]\n2: name = \"nav\""))
    );
}

// ---------------------------------------------------------------------------
// Slice 4: Image part
// ---------------------------------------------------------------------------

#[test]
fn image_part_produces_user_message_with_content_array() {
    let artifact_id = ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").unwrap();
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Image {
            mime: "image/png".to_string(),
            source: ImageSource::FileRef { artifact_id },
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.messages.len(), 1);
    assert_eq!(request.messages[0].role, ChatCompletionMessageRole::User);

    // Content should be a JSON array with image_url type.
    let content = request.messages[0]
        .content
        .as_ref()
        .expect("should have content");
    let arr = content
        .as_array()
        .expect("content should be array for image");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "image_url");
    assert!(
        arr[0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("artifact://")
    );
}

// ---------------------------------------------------------------------------
// Slice 5: Dropped parts (Thinking, StepStart, StepFinish, Retry, Snapshot)
// ---------------------------------------------------------------------------

#[test]
fn thinking_step_retry_snapshot_parts_produce_no_messages() {
    let dropped_parts = vec![
        Part::Thinking {
            text: "reasoning...".to_string(),
            provider_hint: None,
            signature: None,
        },
        Part::StepStart {
            snapshot: Some("before".to_string()),
        },
        Part::StepFinish {
            reason: "tool_use".to_string(),
            cost: 0.5,
            tokens: nav_harness::sessions::TokenUsage::default(),
            snapshot: None,
        },
        Part::Retry {
            attempt: 1,
            error_json: serde_json::json!({ "message": "timeout" }),
        },
        Part::Snapshot {
            snapshot_id: "snap_1".to_string(),
        },
    ];

    for part in dropped_parts {
        let turns = vec![turn(TurnRole::Assistant, 1, vec![part])];
        let request = encode(&turns);
        assert!(
            request.messages.is_empty(),
            "part should produce no messages, got {}",
            request.messages.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Slice 6: Compaction and ProviderOpaque → synthetic text
// ---------------------------------------------------------------------------

#[test]
fn compaction_part_produces_synthetic_user_text_message() {
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Compaction {
            auto: true,
            tail_start_id: Some(
                MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000099").unwrap(),
            ),
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.messages.len(), 1);
    assert_eq!(request.messages[0].role, ChatCompletionMessageRole::User);
    let content = request.messages[0]
        .content
        .as_ref()
        .expect("should have content");
    assert!(
        content.as_str().unwrap().contains("compacted"),
        "should mention compaction"
    );
}

#[test]
fn provider_opaque_part_produces_synthetic_text_message() {
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![Part::ProviderOpaque {
            api_kind: ApiKind::OpenAiCompletions,
            kind: "response.output_item.unknown".to_string(),
            raw_artifact_id: ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").unwrap(),
            raw_payload: Some(
                RawJson::from_string(r#"{"unknown": [true, false]}"#.to_string()).unwrap(),
            ),
        }],
    )];

    let request = encode(&turns);

    assert_eq!(request.messages.len(), 1);
    assert_eq!(
        request.messages[0].role,
        ChatCompletionMessageRole::Assistant
    );
    let content = request.messages[0]
        .content
        .as_ref()
        .expect("should have content");
    assert!(
        content.as_str().unwrap().contains("provider")
            || content.as_str().unwrap().contains("opaque"),
        "should mention provider opaque"
    );
}

// ---------------------------------------------------------------------------
// Slice 7: Multiple parts in one turn → combined message
// ---------------------------------------------------------------------------

#[test]
fn assistant_turn_with_text_and_tool_calls_produces_single_message() {
    let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![
            Part::Text {
                text: "Let me read that file.".to_string(),
                synthetic: None,
            },
            Part::ToolCall {
                id: call_id.clone(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "Cargo.toml"}),
                raw_arguments_artifact_id: None,
            },
        ],
    )];

    let request = encode(&turns);

    assert_eq!(request.messages.len(), 1);
    let msg = &request.messages[0];
    assert_eq!(msg.role, ChatCompletionMessageRole::Assistant);
    assert_eq!(
        msg.content,
        Some(serde_json::json!("Let me read that file."))
    );
    let tool_calls = msg.tool_calls.as_ref().expect("should have tool_calls");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].function.name, "read");
}

#[test]
fn assistant_turn_with_text_and_tool_result_produces_two_messages() {
    let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![
            Part::Text {
                text: "Here is the result:".to_string(),
                synthetic: None,
            },
            Part::ToolResult {
                call_id: call_id.clone(),
                content: "file contents".to_string(),
                raw_artifact_id: None,
                is_error: false,
            },
        ],
    )];

    let request = encode(&turns);

    // Text part → assistant message, ToolResult → tool message.
    assert_eq!(request.messages.len(), 2);
    assert_eq!(
        request.messages[0].role,
        ChatCompletionMessageRole::Assistant
    );
    assert_eq!(
        request.messages[0].content,
        Some(serde_json::json!("Here is the result:"))
    );
    assert_eq!(request.messages[1].role, ChatCompletionMessageRole::Tool);
    assert_eq!(
        request.messages[1].tool_call_id.as_deref(),
        Some(call_id.as_str())
    );
}

#[test]
fn multiple_tool_results_in_one_turn_produce_separate_messages() {
    let call_a = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000051").unwrap();
    let call_b = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000052").unwrap();
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![
            Part::ToolResult {
                call_id: call_a.clone(),
                content: "result A".to_string(),
                raw_artifact_id: None,
                is_error: false,
            },
            Part::ToolResult {
                call_id: call_b.clone(),
                content: "result B".to_string(),
                raw_artifact_id: None,
                is_error: true,
            },
        ],
    )];

    let request = encode(&turns);

    assert_eq!(request.messages.len(), 2);
    assert_eq!(
        request.messages[0].tool_call_id.as_deref(),
        Some(call_a.as_str())
    );
    assert_eq!(
        request.messages[0].content,
        Some(serde_json::json!("result A"))
    );
    assert_eq!(
        request.messages[1].tool_call_id.as_deref(),
        Some(call_b.as_str())
    );
    assert_eq!(
        request.messages[1].content,
        Some(serde_json::json!("result B"))
    );
}

// ---------------------------------------------------------------------------
// Slice 8: Tools array
// ---------------------------------------------------------------------------

#[test]
fn encoder_with_tools_includes_them_in_request() {
    let tools = vec![ChatCompletionToolDefinition {
        name: "read".to_string(),
        description: "Read a file".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        }),
    }];

    let encoder = OpenAiChatCompletionsEncoder::new().with_tools(tools);
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Text {
            text: "Read Cargo.toml".to_string(),
            synthetic: None,
        }],
    )];

    let request = encoder.encode(&turns).expect("should encode");

    assert_eq!(request.tools.len(), 1);
    assert_eq!(request.tools[0].name, "read");
}

#[test]
fn encoder_without_tools_produces_empty_tools() {
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Text {
            text: "hello".to_string(),
            synthetic: None,
        }],
    )];

    let request = encode(&turns);

    assert!(request.tools.is_empty());
}

// ---------------------------------------------------------------------------
// Slice 9: Compat flags
// ---------------------------------------------------------------------------

#[test]
fn encoder_produces_request_without_provider_state() {
    let turns = vec![turn(
        TurnRole::User,
        1,
        vec![Part::Text {
            text: "hello".to_string(),
            synthetic: None,
        }],
    )];

    let request = encode(&turns);

    // Encoder produces a pure request; compat flags are applied downstream
    // by request_body() during JSON serialization.
    assert_eq!(request.messages.len(), 1);
    assert!(request.tools.is_empty());
}

// ---------------------------------------------------------------------------
// Slice 10: Snapshot — full request body fixture
// ---------------------------------------------------------------------------

#[test]
fn full_request_body_matches_known_good_fixture() {
    let call_id = ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
    let artifact_id = ArtifactId::try_new("art_0000018bcfe56800_0000000000000001").unwrap();

    let tools = vec![ChatCompletionToolDefinition {
        name: "read".to_string(),
        description: "Read a file".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        }),
    }];

    let encoder = OpenAiChatCompletionsEncoder::new().with_tools(tools);

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
                    source: nav_harness::sessions::ImageSource::FileRef { artifact_id },
                },
            ],
        ),
        turn(
            TurnRole::Assistant,
            2,
            vec![
                Part::Text {
                    text: "Let me read that.".to_string(),
                    synthetic: None,
                },
                Part::ToolCall {
                    id: call_id.clone(),
                    name: "read".to_string(),
                    arguments: serde_json::json!({"path": "Cargo.toml"}),
                    raw_arguments_artifact_id: None,
                },
                Part::Thinking {
                    text: "considering options".to_string(),
                    provider_hint: None,
                    signature: None,
                },
            ],
        ),
        turn(
            TurnRole::Assistant,
            3,
            vec![Part::ToolResult {
                call_id: call_id.clone(),
                content: "1: [package]\n2: name = \"nav\"".to_string(),
                raw_artifact_id: None,
                is_error: false,
            }],
        ),
        turn(
            TurnRole::Assistant,
            4,
            vec![
                Part::Compaction {
                    auto: true,
                    tail_start_id: None,
                },
                Part::Text {
                    text: "Here is the file.".to_string(),
                    synthetic: None,
                },
            ],
        ),
    ];

    let request = encoder.encode(&turns).expect("should encode");

    // Verify the full structure.
    assert_eq!(request.messages.len(), 4, "should have 4 messages");

    // Message 0: user with text + image (multimodal content array)
    assert_eq!(request.messages[0].role, ChatCompletionMessageRole::User);
    let content0 = request.messages[0].content.as_ref().unwrap();
    let arr0 = content0
        .as_array()
        .expect("multimodal content should be array");
    assert_eq!(arr0.len(), 2);
    assert_eq!(arr0[0]["type"], "text");
    assert_eq!(arr0[0]["text"], "Read Cargo.toml");
    assert_eq!(arr0[1]["type"], "image_url");

    // Message 1: assistant with text + tool_calls (Thinking dropped)
    assert_eq!(
        request.messages[1].role,
        ChatCompletionMessageRole::Assistant
    );
    assert_eq!(
        request.messages[1].content,
        Some(serde_json::json!("Let me read that."))
    );
    let tc = request.messages[1].tool_calls.as_ref().unwrap();
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].function.name, "read");

    // Message 2: tool result
    assert_eq!(request.messages[2].role, ChatCompletionMessageRole::Tool);
    assert_eq!(
        request.messages[2].tool_call_id.as_deref(),
        Some(call_id.as_str())
    );

    // Message 3: compaction + text combined (synthetic + real text)
    assert_eq!(
        request.messages[3].role,
        ChatCompletionMessageRole::Assistant
    );
    let content3 = request.messages[3]
        .content
        .as_ref()
        .unwrap()
        .as_str()
        .unwrap();
    assert!(content3.contains("compacted"));
    assert!(content3.contains("Here is the file."));

    // Tools included.
    assert_eq!(request.tools.len(), 1);
    assert_eq!(request.tools[0].name, "read");
}
