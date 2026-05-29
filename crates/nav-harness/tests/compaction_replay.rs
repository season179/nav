//! Replay projection tests for compaction-safe provider requests.

use nav_harness::compaction::replay::project_for_replay;
use nav_harness::models::{
    AnthropicMessagesEncoder, ApiKind, ChatCompletionMessageRole, OpenAiChatCompletionsEncoder,
    OpenAiResponsesEncoder,
};
use nav_harness::sessions::{ImageSource, Part, StoredPart, Turn, TurnMeta, TurnRole};
use nav_types::{ArtifactId, MessageId, PartId, RunId, ToolCallId};

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test message id should be UUIDv7-shaped")
}

fn run_id(suffix: u64) -> RunId {
    RunId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test run id should be UUIDv7-shaped")
}

fn part_id(suffix: u64) -> PartId {
    PartId::try_new(format!("prt_0000018bcfe56800_{suffix:016x}"))
        .expect("test part id should be storage-id-shaped")
}

fn tool_call_id(suffix: u64) -> ToolCallId {
    ToolCallId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test tool call id should be UUIDv7-shaped")
}

fn artifact_id(suffix: u64) -> ArtifactId {
    ArtifactId::try_new(format!("art_0000018bcfe56800_{suffix:016x}"))
        .expect("test artifact id should be storage-id-shaped")
}

fn turn(role: TurnRole, seq: u32, parts: Vec<StoredPart>) -> (Turn, Vec<StoredPart>) {
    (
        Turn {
            id: message_id(seq as u64),
            run_id: run_id(1),
            seq,
            role,
            meta: TurnMeta::default(),
            created_at: 1_700_000_000_000 + i64::from(seq),
        },
        parts,
    )
}

fn stored_part(id: PartId, part: Part, compacted_at: Option<i64>) -> StoredPart {
    StoredPart {
        id,
        part,
        provider_payload_id: None,
        provider_json_pointer: None,
        compacted_at,
        created_at: 1_700_000_000_000,
    }
}

#[test]
fn compacted_tool_result_replays_placeholder_content() {
    let call_id = tool_call_id(50);
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![stored_part(
            part_id(1),
            Part::ToolResult {
                call_id: call_id.clone(),
                content: "large output".to_string(),
                raw_artifact_id: None,
                is_error: false,
            },
            Some(1_700_000_000_999),
        )],
    )];

    let projected = project_for_replay(&turns, 0);

    assert_eq!(projected.len(), 1);
    assert_eq!(
        projected[0].1,
        vec![Part::ToolResult {
            call_id,
            content: "[Old tool result content cleared]".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }]
    );
}

#[test]
fn duplicate_tool_result_replays_placeholder_on_older_copy() {
    let older_call_id = tool_call_id(51);
    let newer_call_id = tool_call_id(52);
    let turns = vec![
        turn(
            TurnRole::Assistant,
            1,
            vec![stored_part(
                part_id(1),
                Part::ToolResult {
                    call_id: older_call_id.clone(),
                    content: "same file contents".to_string(),
                    raw_artifact_id: None,
                    is_error: false,
                },
                None,
            )],
        ),
        turn(
            TurnRole::Assistant,
            2,
            vec![stored_part(
                part_id(2),
                Part::ToolResult {
                    call_id: newer_call_id.clone(),
                    content: "same file contents".to_string(),
                    raw_artifact_id: None,
                    is_error: false,
                },
                None,
            )],
        ),
    ];

    let projected = project_for_replay(&turns, 0);

    assert_eq!(
        projected[0].1,
        vec![Part::ToolResult {
            call_id: older_call_id,
            content: "[Duplicate — see more recent result]".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }]
    );
    assert_eq!(
        projected[1].1,
        vec![Part::ToolResult {
            call_id: newer_call_id,
            content: "same file contents".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }]
    );
}

#[test]
fn tool_call_argument_projection_truncates_long_strings_without_changing_json_shape() {
    let call_id = tool_call_id(53);
    let raw_arguments_artifact_id = artifact_id(1);
    let long_body = "x".repeat(3_000);
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![stored_part(
            part_id(1),
            Part::ToolCall {
                id: call_id.clone(),
                name: "write".to_string(),
                arguments: serde_json::json!({
                    "path": "src/main.rs",
                    "body": long_body,
                    "metadata": {
                        "mode": "replace"
                    }
                }),
                raw_arguments_artifact_id: Some(raw_arguments_artifact_id.clone()),
            },
            None,
        )],
    )];

    let projected = project_for_replay(&turns, 0);
    let Part::ToolCall {
        id,
        name,
        arguments,
        raw_arguments_artifact_id: projected_raw_arguments_artifact_id,
    } = &projected[0].1[0]
    else {
        panic!("expected projected tool call");
    };

    assert_eq!(id, &call_id);
    assert_eq!(name, "write");
    assert_eq!(
        projected_raw_arguments_artifact_id.as_ref(),
        Some(&raw_arguments_artifact_id)
    );
    assert_eq!(arguments["path"], "src/main.rs");
    assert_eq!(arguments["metadata"]["mode"], "replace");

    let projected_body = arguments["body"]
        .as_str()
        .expect("body should remain a JSON string");
    assert!(projected_body.ends_with("... [truncated]"));
    assert!(projected_body.len() < 3_000);
}

#[test]
fn tool_call_argument_projection_truncates_unspilled_long_strings() {
    let long_body = "x".repeat(3_000);
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![stored_part(
            part_id(1),
            Part::ToolCall {
                id: tool_call_id(55),
                name: "write".to_string(),
                arguments: serde_json::json!({
                    "body": long_body,
                }),
                raw_arguments_artifact_id: None,
            },
            None,
        )],
    )];

    let projected = project_for_replay(&turns, 0);
    let Part::ToolCall { arguments, .. } = &projected[0].1[0] else {
        panic!("expected projected tool call");
    };
    let projected_body = arguments["body"]
        .as_str()
        .expect("body should remain a JSON string");

    assert!(projected_body.ends_with("... [truncated]"));
    assert!(projected_body.len() < 3_000);
}

#[test]
fn tool_call_argument_projection_never_expands_barely_long_strings() {
    let long_body = "x".repeat(1_025);
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![stored_part(
            part_id(1),
            Part::ToolCall {
                id: tool_call_id(58),
                name: "write".to_string(),
                arguments: serde_json::json!({
                    "body": long_body,
                }),
                raw_arguments_artifact_id: None,
            },
            None,
        )],
    )];

    let projected = project_for_replay(&turns, 0);
    let Part::ToolCall { arguments, .. } = &projected[0].1[0] else {
        panic!("expected projected tool call");
    };
    let projected_body = arguments["body"]
        .as_str()
        .expect("body should remain a JSON string");

    assert!(projected_body.ends_with("... [truncated]"));
    assert!(projected_body.len() <= 1_024);
}

#[test]
fn image_projection_strips_images_before_latest_image_bearing_user_turn() {
    let older_artifact_id = artifact_id(1);
    let newer_artifact_id = artifact_id(2);
    let turns = vec![
        turn(
            TurnRole::User,
            1,
            vec![stored_part(
                part_id(1),
                Part::Image {
                    mime: "image/png".to_string(),
                    source: ImageSource::FileRef {
                        artifact_id: older_artifact_id,
                    },
                },
                None,
            )],
        ),
        turn(
            TurnRole::User,
            2,
            vec![stored_part(
                part_id(2),
                Part::Image {
                    mime: "image/png".to_string(),
                    source: ImageSource::FileRef {
                        artifact_id: newer_artifact_id.clone(),
                    },
                },
                None,
            )],
        ),
    ];

    let projected = project_for_replay(&turns, 0);

    assert_eq!(
        projected[0].1,
        vec![Part::Text {
            text: "[Attached image — stripped after compression]".to_string(),
            synthetic: Some(true),
        }]
    );
    assert_eq!(
        projected[1].1,
        vec![Part::Image {
            mime: "image/png".to_string(),
            source: ImageSource::FileRef {
                artifact_id: newer_artifact_id,
            },
        }]
    );
}

#[test]
fn provider_opaque_projection_includes_kind_and_artifact_back_reference() {
    let raw_artifact_id = artifact_id(7);
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![stored_part(
            part_id(1),
            Part::ProviderOpaque {
                api_kind: ApiKind::OpenAiResponses,
                kind: "response.output_item.unknown".to_string(),
                raw_artifact_id: raw_artifact_id.clone(),
                raw_payload: None,
            },
            None,
        )],
    )];

    let projected = project_for_replay(&turns, 0);

    assert_eq!(
        projected[0].1,
        vec![Part::Text {
            text: format!(
                "[Provider-specific content: response.output_item.unknown; raw artifact: {raw_artifact_id}]"
            ),
            synthetic: Some(true),
        }]
    );
}

#[test]
fn projected_turns_reencode_to_chat_completions_request() {
    let call_id = tool_call_id(54);
    let turns = tool_call_then_compacted_result_turns(call_id.clone());

    let projected = project_for_replay(&turns, 0);
    let request = OpenAiChatCompletionsEncoder::new()
        .encode(&projected)
        .expect("projected turns should encode");

    assert_eq!(request.messages.len(), 2);
    assert_eq!(
        request.messages[0].role,
        ChatCompletionMessageRole::Assistant
    );
    assert_eq!(request.messages[1].role, ChatCompletionMessageRole::Tool);
    assert_eq!(
        request.messages[1].tool_call_id.as_deref(),
        Some(call_id.as_str())
    );
    assert_eq!(
        request.messages[1].content,
        Some(serde_json::json!("[Old tool result content cleared]"))
    );
}

#[test]
fn projected_turns_reencode_to_responses_request() {
    let call_id = tool_call_id(56);
    let turns = tool_call_then_compacted_result_turns(call_id.clone());

    let projected = project_for_replay(&turns, 0);
    let request = OpenAiResponsesEncoder::new()
        .encode(&projected)
        .expect("projected turns should encode");

    assert_eq!(request.input.len(), 2);
    assert_eq!(request.input[0]["type"], "function_call");
    assert_eq!(request.input[1]["type"], "function_call_output");
    assert_eq!(request.input[1]["call_id"], call_id.as_str());
    assert_eq!(
        request.input[1]["output"],
        "[Old tool result content cleared]"
    );
}

#[test]
fn projected_turns_reencode_to_anthropic_messages_request() {
    let call_id = tool_call_id(57);
    let turns = tool_call_then_compacted_result_turns(call_id.clone());

    let projected = project_for_replay(&turns, 0);
    let request = AnthropicMessagesEncoder::new()
        .encode(&projected)
        .expect("projected turns should encode");

    assert_eq!(request.messages.len(), 2);
    assert_eq!(request.messages[0]["role"], "assistant");
    assert_eq!(request.messages[1]["role"], "user");
    assert_eq!(
        request.messages[1]["content"][0]["tool_use_id"],
        call_id.as_str()
    );
    assert_eq!(
        request.messages[1]["content"][0]["content"],
        "[Old tool result content cleared]"
    );
}

fn tool_call_then_compacted_result_turns(call_id: ToolCallId) -> Vec<(Turn, Vec<StoredPart>)> {
    vec![
        turn(
            TurnRole::Assistant,
            1,
            vec![stored_part(
                part_id(1),
                Part::ToolCall {
                    id: call_id.clone(),
                    name: "read".to_string(),
                    arguments: serde_json::json!({"path": "Cargo.toml"}),
                    raw_arguments_artifact_id: None,
                },
                None,
            )],
        ),
        turn(
            TurnRole::Assistant,
            2,
            vec![stored_part(
                part_id(2),
                Part::ToolResult {
                    call_id,
                    content: "large output".to_string(),
                    raw_artifact_id: None,
                    is_error: false,
                },
                Some(1_700_000_000_999),
            )],
        ),
    ]
}

#[test]
fn protected_tail_preserves_arguments_in_recent_turns_but_truncates_older() {
    let old_long_body = "a".repeat(3_000);
    let new_long_body = "b".repeat(3_000);
    let turns = vec![
        turn(
            TurnRole::Assistant,
            0,
            vec![stored_part(
                part_id(1),
                Part::ToolCall {
                    id: tool_call_id(100),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"path": "old.rs", "body": old_long_body}),
                    raw_arguments_artifact_id: Some(artifact_id(10)),
                },
                None,
            )],
        ),
        turn(
            TurnRole::Assistant,
            1,
            vec![stored_part(
                part_id(2),
                Part::ToolCall {
                    id: tool_call_id(101),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"path": "new.rs", "body": new_long_body.clone()}),
                    raw_arguments_artifact_id: Some(artifact_id(11)),
                },
                None,
            )],
        ),
    ];

    // protected_tail_turns = 1 → only the last assistant turn is preserved
    let projected = project_for_replay(&turns, 1);

    // Old turn: truncated
    let Part::ToolCall {
        arguments: old_args,
        ..
    } = &projected[0].1[0]
    else {
        panic!("expected tool call");
    };
    let old_body = old_args["body"].as_str().expect("body should be string");
    assert!(old_body.ends_with("... [truncated]"));
    assert!(old_body.len() < 3_000);

    // New turn (in protected tail): preserved
    let Part::ToolCall {
        arguments: new_args,
        ..
    } = &projected[1].1[0]
    else {
        panic!("expected tool call");
    };
    let new_body = new_args["body"].as_str().expect("body should be string");
    assert_eq!(new_body, new_long_body);
}

#[test]
fn protected_tail_of_zero_truncates_all_turns() {
    let long_body = "x".repeat(3_000);
    let turns = vec![turn(
        TurnRole::Assistant,
        0,
        vec![stored_part(
            part_id(1),
            Part::ToolCall {
                id: tool_call_id(110),
                name: "write".to_string(),
                arguments: serde_json::json!({"body": long_body}),
                raw_arguments_artifact_id: None,
            },
            None,
        )],
    )];

    let projected = project_for_replay(&turns, 0);

    let Part::ToolCall { arguments, .. } = &projected[0].1[0] else {
        panic!("expected tool call");
    };
    let body = arguments["body"].as_str().expect("body should be string");
    assert!(body.ends_with("... [truncated]"));
    assert!(body.len() < 3_000);
}

#[test]
fn protected_tail_larger_than_turn_count_preserves_all() {
    let long_body = "z".repeat(3_000);
    let turns = vec![turn(
        TurnRole::Assistant,
        0,
        vec![stored_part(
            part_id(1),
            Part::ToolCall {
                id: tool_call_id(120),
                name: "write".to_string(),
                arguments: serde_json::json!({"body": long_body.clone()}),
                raw_arguments_artifact_id: None,
            },
            None,
        )],
    )];

    // 1 turn, tail of 10 → all preserved
    let projected = project_for_replay(&turns, 10);

    let Part::ToolCall { arguments, .. } = &projected[0].1[0] else {
        panic!("expected tool call");
    };
    let body = arguments["body"].as_str().expect("body should be string");
    assert_eq!(body, long_body);
}

#[test]
fn protected_tail_counts_only_assistant_turns_for_tail_size() {
    let long_body_1 = "a".repeat(3_000);
    let long_body_2 = "b".repeat(3_000);
    let long_body_3 = "c".repeat(3_000);
    let turns = vec![
        // Assistant turn 0 (old, should be truncated)
        turn(
            TurnRole::Assistant,
            0,
            vec![stored_part(
                part_id(1),
                Part::ToolCall {
                    id: tool_call_id(130),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"body": long_body_1}),
                    raw_arguments_artifact_id: None,
                },
                None,
            )],
        ),
        // User turn 1 (doesn't count toward tail)
        turn(
            TurnRole::User,
            1,
            vec![stored_part(
                part_id(2),
                Part::Text {
                    text: "user message".to_string(),
                    synthetic: None,
                },
                None,
            )],
        ),
        // Assistant turn 2 (should be in tail = preserved)
        turn(
            TurnRole::Assistant,
            2,
            vec![stored_part(
                part_id(3),
                Part::ToolCall {
                    id: tool_call_id(131),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"body": long_body_2.clone()}),
                    raw_arguments_artifact_id: None,
                },
                None,
            )],
        ),
        // Assistant turn 3 (should be in tail = preserved)
        turn(
            TurnRole::Assistant,
            3,
            vec![stored_part(
                part_id(4),
                Part::ToolCall {
                    id: tool_call_id(132),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"body": long_body_3.clone()}),
                    raw_arguments_artifact_id: None,
                },
                None,
            )],
        ),
    ];

    // protected_tail_turns = 2 → last 2 assistant turns preserved
    let projected = project_for_replay(&turns, 2);

    // Turn 0 (old assistant): truncated
    let Part::ToolCall { arguments, .. } = &projected[0].1[0] else {
        panic!("expected tool call");
    };
    assert!(
        arguments["body"]
            .as_str()
            .unwrap()
            .ends_with("... [truncated]")
    );

    // Turn 2 (assistant, in tail): preserved
    let Part::ToolCall { arguments, .. } = &projected[2].1[0] else {
        panic!("expected tool call");
    };
    assert_eq!(arguments["body"].as_str().unwrap(), long_body_2);

    // Turn 3 (assistant, in tail): preserved
    let Part::ToolCall { arguments, .. } = &projected[3].1[0] else {
        panic!("expected tool call");
    };
    assert_eq!(arguments["body"].as_str().unwrap(), long_body_3);
}

#[test]
fn protected_tail_at_50kb_acceptance_criteria() {
    let large_body = "x".repeat(50 * 1024);
    let turns = vec![turn(
        TurnRole::Assistant,
        0,
        vec![stored_part(
            part_id(1),
            Part::ToolCall {
                id: tool_call_id(140),
                name: "write_file".to_string(),
                arguments: serde_json::json!({"path": "src/main.rs", "content": large_body}),
                raw_arguments_artifact_id: Some(artifact_id(20)),
            },
            None,
        )],
    )];

    let projected = project_for_replay(&turns, 0);

    let Part::ToolCall {
        arguments,
        raw_arguments_artifact_id,
        ..
    } = &projected[0].1[0]
    else {
        panic!("expected tool call");
    };
    let content = arguments["content"]
        .as_str()
        .expect("content should be string");
    assert!(content.ends_with("... [truncated]"));
    assert!(content.len() < 50 * 1024);
    // Path field preserved
    assert_eq!(arguments["path"], "src/main.rs");
    // Artifact ID preserved for retrieval
    assert!(raw_arguments_artifact_id.is_some());
}

#[test]
fn protected_tail_preserves_all_tool_calls_in_protected_turn() {
    let long_body_a = "a".repeat(3_000);
    let long_body_b = "b".repeat(3_000);
    let turns = vec![
        // Old assistant turn with one tool call (should be truncated)
        turn(
            TurnRole::Assistant,
            0,
            vec![stored_part(
                part_id(1),
                Part::ToolCall {
                    id: tool_call_id(150),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"body": long_body_a}),
                    raw_arguments_artifact_id: None,
                },
                None,
            )],
        ),
        // Recent assistant turn with TWO tool calls (both should be preserved)
        turn(
            TurnRole::Assistant,
            1,
            vec![
                stored_part(
                    part_id(2),
                    Part::ToolCall {
                        id: tool_call_id(151),
                        name: "write".to_string(),
                        arguments: serde_json::json!({"body": long_body_b.clone()}),
                        raw_arguments_artifact_id: None,
                    },
                    None,
                ),
                stored_part(
                    part_id(3),
                    Part::ToolCall {
                        id: tool_call_id(152),
                        name: "edit".to_string(),
                        arguments: serde_json::json!({"path": "src/lib.rs", "body": long_body_b.clone()}),
                        raw_arguments_artifact_id: None,
                    },
                    None,
                ),
            ],
        ),
    ];

    // Tail of 1 → last assistant turn preserved
    let projected = project_for_replay(&turns, 1);

    // Old turn: truncated
    let Part::ToolCall { arguments, .. } = &projected[0].1[0] else {
        panic!("expected tool call");
    };
    assert!(
        arguments["body"]
            .as_str()
            .unwrap()
            .ends_with("... [truncated]")
    );

    // Recent turn: both tool calls preserved
    assert_eq!(projected[1].1.len(), 2);
    let Part::ToolCall { arguments, .. } = &projected[1].1[0] else {
        panic!("expected tool call");
    };
    assert_eq!(arguments["body"].as_str().unwrap(), long_body_b);
    let Part::ToolCall { arguments, .. } = &projected[1].1[1] else {
        panic!("expected tool call");
    };
    assert_eq!(arguments["body"].as_str().unwrap(), long_body_b);
}
