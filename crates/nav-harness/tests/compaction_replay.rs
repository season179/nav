//! Replay projection tests for compaction-safe provider requests.

use nav_harness::compaction::replay::{DEFAULT_TAIL_TURNS, project_for_replay};
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

fn image_user_turn(
    seq: u32,
    caption: &str,
    image_artifact_id: ArtifactId,
) -> (Turn, Vec<StoredPart>) {
    turn(
        TurnRole::User,
        seq,
        vec![
            stored_part(
                part_id(u64::from(seq) * 2),
                Part::Text {
                    text: caption.to_string(),
                    synthetic: None,
                },
                None,
            ),
            stored_part(
                part_id(u64::from(seq) * 2 + 1),
                Part::Image {
                    mime: "image/png".to_string(),
                    source: ImageSource::FileRef {
                        artifact_id: image_artifact_id,
                    },
                },
                None,
            ),
        ],
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
fn compaction_marker_replays_summary_then_tail() {
    let tail_start_id = message_id(9);
    let mut turns = (1..=10)
        .map(|seq| {
            turn(
                if seq % 2 == 0 {
                    TurnRole::Assistant
                } else {
                    TurnRole::User
                },
                seq,
                vec![stored_part(
                    part_id(seq as u64),
                    Part::Text {
                        text: format!("turn {seq}"),
                        synthetic: None,
                    },
                    None,
                )],
            )
        })
        .collect::<Vec<_>>();

    turns.push(turn(
        TurnRole::User,
        11,
        vec![stored_part(
            part_id(11),
            Part::Compaction {
                auto: true,
                tail_start_id: Some(tail_start_id.clone()),
            },
            None,
        )],
    ));
    turns.push(turn(
        TurnRole::Assistant,
        12,
        vec![stored_part(
            part_id(12),
            Part::Text {
                text: "summary pending".to_string(),
                synthetic: Some(true),
            },
            None,
        )],
    ));

    let projected = project_for_replay(&turns, DEFAULT_TAIL_TURNS);
    let projected_ids = projected
        .iter()
        .map(|(turn, _)| turn.id.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        projected_ids,
        vec![
            message_id(11),
            message_id(12),
            message_id(9),
            message_id(10)
        ]
    );
    assert_eq!(
        projected[0].1,
        vec![Part::Compaction {
            auto: true,
            tail_start_id: Some(tail_start_id),
        }]
    );
}

#[test]
fn compaction_without_tail_still_replays_future_turns() {
    let turns = vec![
        turn(
            TurnRole::User,
            1,
            vec![stored_part(
                part_id(1),
                Part::Compaction {
                    auto: true,
                    tail_start_id: None,
                },
                None,
            )],
        ),
        turn(
            TurnRole::Assistant,
            2,
            vec![stored_part(
                part_id(2),
                Part::Text {
                    text: "summary pending".to_string(),
                    synthetic: Some(true),
                },
                None,
            )],
        ),
        turn(
            TurnRole::User,
            3,
            vec![stored_part(
                part_id(3),
                Part::Text {
                    text: "future user turn".to_string(),
                    synthetic: None,
                },
                None,
            )],
        ),
    ];

    let projected = project_for_replay(&turns, DEFAULT_TAIL_TURNS);
    let projected_ids = projected
        .iter()
        .map(|(turn, _)| turn.id.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        projected_ids,
        vec![message_id(1), message_id(2), message_id(3)]
    );
}

#[test]
fn compaction_with_missing_tail_start_does_not_replay_full_history() {
    let turns = vec![
        turn(
            TurnRole::User,
            1,
            vec![stored_part(
                part_id(1),
                Part::Text {
                    text: "old user turn".to_string(),
                    synthetic: None,
                },
                None,
            )],
        ),
        turn(
            TurnRole::User,
            2,
            vec![stored_part(
                part_id(2),
                Part::Compaction {
                    auto: true,
                    tail_start_id: Some(message_id(99)),
                },
                None,
            )],
        ),
        turn(
            TurnRole::Assistant,
            3,
            vec![stored_part(
                part_id(3),
                Part::Text {
                    text: "summary pending".to_string(),
                    synthetic: Some(true),
                },
                None,
            )],
        ),
        turn(
            TurnRole::User,
            4,
            vec![stored_part(
                part_id(4),
                Part::Text {
                    text: "future user turn".to_string(),
                    synthetic: None,
                },
                None,
            )],
        ),
    ];

    let projected = project_for_replay(&turns, DEFAULT_TAIL_TURNS);
    let projected_ids = projected
        .iter()
        .map(|(turn, _)| turn.id.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        projected_ids,
        vec![message_id(2), message_id(3), message_id(4)]
    );
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

    let projected = project_all(&turns);

    assert_eq!(projected.len(), 1);
    assert_eq!(
        projected[0].1,
        vec![Part::ToolResult {
            call_id,
            content: "[unknown tool]: 12 chars.".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }]
    );
}

#[test]
fn compacted_tool_result_replays_summary_with_tool_name() {
    let call_id = tool_call_id(80);
    let content = "x".repeat(5_000);
    let turns = vec![
        turn(
            TurnRole::Assistant,
            1,
            vec![stored_part(
                part_id(80),
                Part::ToolCall {
                    id: call_id.clone(),
                    name: "bash".to_string(),
                    arguments: serde_json::json!({"command": "ls -la"}),
                    raw_arguments_artifact_id: None,
                },
                None,
            )],
        ),
        turn(
            TurnRole::Assistant,
            2,
            vec![stored_part(
                part_id(81),
                Part::ToolResult {
                    call_id: call_id.clone(),
                    content: content.clone(),
                    raw_artifact_id: None,
                    is_error: false,
                },
                Some(1_700_000_000_999),
            )],
        ),
    ];

    let projected = project_all(&turns);

    assert_eq!(projected.len(), 2);
    assert_eq!(
        projected[1].1,
        vec![Part::ToolResult {
            call_id,
            content: format!("[bash]: {} chars.", content.len()),
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

    let projected = project_all(&turns);

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
fn different_tool_result_contents_are_not_deduped() {
    let turns = vec![
        turn(
            TurnRole::Assistant,
            1,
            vec![stored_part(
                part_id(1),
                Part::ToolResult {
                    call_id: tool_call_id(60),
                    content: "file version A".to_string(),
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
                    call_id: tool_call_id(61),
                    content: "file version B".to_string(),
                    raw_artifact_id: None,
                    is_error: false,
                },
                None,
            )],
        ),
    ];

    let projected = project_all(&turns);

    // Both should be kept as full content — not deduped
    assert_eq!(
        projected[0].1[0],
        Part::ToolResult {
            call_id: tool_call_id(60),
            content: "file version A".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }
    );
    assert_eq!(
        projected[1].1[0],
        Part::ToolResult {
            call_id: tool_call_id(61),
            content: "file version B".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }
    );
}

#[test]
fn five_identical_tool_results_leave_one_full_copy_and_four_back_references() {
    let file_content = "fn main() { println!(\"hello\"); }";
    let mut stored_turns = Vec::new();
    for i in 0..5u64 {
        let call_id = tool_call_id(100 + i);
        let raw_artifact_id = artifact_id(100 + i);
        stored_turns.push(turn(
            TurnRole::Assistant,
            i as u32,
            vec![stored_part(
                part_id(100 + i),
                Part::ToolResult {
                    call_id: call_id.clone(),
                    content: file_content.to_string(),
                    raw_artifact_id: Some(raw_artifact_id),
                    is_error: false,
                },
                None,
            )],
        ));
    }

    let projected = project_all(&stored_turns);

    // Newest (last) copy kept as full content
    let last_parts = &projected[4].1;
    assert_eq!(
        last_parts[0],
        Part::ToolResult {
            call_id: tool_call_id(104),
            content: file_content.to_string(),
            raw_artifact_id: Some(artifact_id(104)),
            is_error: false,
        },
        "newest copy should retain full content"
    );

    // Older 4 copies replaced with back-reference
    for (i, (_, parts)) in projected.iter().enumerate().take(4) {
        assert_eq!(
            parts[0],
            Part::ToolResult {
                call_id: tool_call_id(100 + i as u64),
                content: "[Duplicate — see more recent result]".to_string(),
                raw_artifact_id: Some(artifact_id(100 + i as u64)),
                is_error: false,
            },
            "older copy {i} should be a back-reference"
        );
    }
}

#[test]
fn compacted_parts_excluded_from_hash_dedup() {
    let file_content = "shared output";
    let turns = vec![
        turn(
            TurnRole::Assistant,
            1,
            vec![stored_part(
                part_id(1),
                Part::ToolResult {
                    call_id: tool_call_id(70),
                    content: file_content.to_string(),
                    raw_artifact_id: None,
                    is_error: false,
                },
                Some(1_700_000_000_999), // compacted
            )],
        ),
        turn(
            TurnRole::Assistant,
            2,
            vec![stored_part(
                part_id(2),
                Part::ToolResult {
                    call_id: tool_call_id(71),
                    content: file_content.to_string(),
                    raw_artifact_id: None,
                    is_error: false,
                },
                None, // live
            )],
        ),
    ];

    let projected = project_all(&turns);

    // Compacted part gets the compacted placeholder, not the dedup placeholder
    assert_eq!(
        projected[0].1[0],
        Part::ToolResult {
            call_id: tool_call_id(70),
            content: "[unknown tool]: 13 chars.".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }
    );
    // Live part keeps full content (not marked as duplicate)
    assert_eq!(
        projected[1].1[0],
        Part::ToolResult {
            call_id: tool_call_id(71),
            content: file_content.to_string(),
            raw_artifact_id: None,
            is_error: false,
        }
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

    let projected = project_all(&turns);
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

    let projected = project_all(&turns);
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

    let projected = project_all(&turns);
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
    let oldest_artifact_id = artifact_id(1);
    let middle_artifact_id = artifact_id(2);
    let newest_artifact_id = artifact_id(3);
    let turns = vec![
        turn(
            TurnRole::User,
            1,
            vec![stored_part(
                part_id(1),
                Part::Image {
                    mime: "image/png".to_string(),
                    source: ImageSource::FileRef {
                        artifact_id: oldest_artifact_id,
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
                        artifact_id: middle_artifact_id.clone(),
                    },
                },
                None,
            )],
        ),
        turn(
            TurnRole::User,
            3,
            vec![stored_part(
                part_id(3),
                Part::Image {
                    mime: "image/png".to_string(),
                    source: ImageSource::FileRef {
                        artifact_id: newest_artifact_id.clone(),
                    },
                },
                None,
            )],
        ),
    ];

    let projected = project_all(&turns);

    // Oldest image stripped (outside keep_media_turns=2 window)
    assert_eq!(
        projected[0].1,
        vec![Part::Text {
            text: "[image elided]".to_string(),
            synthetic: Some(true),
        }]
    );
    // Middle and newest images kept (within window)
    assert_eq!(
        projected[1].1,
        vec![Part::Image {
            mime: "image/png".to_string(),
            source: ImageSource::FileRef {
                artifact_id: middle_artifact_id,
            },
        }]
    );
    assert_eq!(
        projected[2].1,
        vec![Part::Image {
            mime: "image/png".to_string(),
            source: ImageSource::FileRef {
                artifact_id: newest_artifact_id,
            },
        }]
    );
}

#[test]
fn image_stripping_keeps_last_keep_media_turns_images() {
    let art_1 = artifact_id(1);
    let art_2 = artifact_id(2);
    let art_3 = artifact_id(3);
    let turns = vec![
        image_user_turn(1, "first screenshot", art_1.clone()),
        image_user_turn(2, "second screenshot", art_2.clone()),
        image_user_turn(3, "third screenshot", art_3.clone()),
    ];

    // With the default keep_media_turns=2, the last 2 image-bearing turns
    // keep their images; the oldest is stripped.
    let projected = project_for_replay(&turns, DEFAULT_TAIL_TURNS);

    let stripped = Part::Text {
        text: "[image elided]".to_string(),
        synthetic: Some(true),
    };
    // First turn: image stripped
    assert_eq!(projected[0].1.len(), 2);
    assert_eq!(
        projected[0].1[0],
        Part::Text {
            text: "first screenshot".to_string(),
            synthetic: None
        }
    );
    assert_eq!(projected[0].1[1], stripped);
    // Second turn: image kept (within last 2)
    assert_eq!(
        projected[1].1[1],
        Part::Image {
            mime: "image/png".to_string(),
            source: ImageSource::FileRef { artifact_id: art_2 },
        }
    );
    // Third turn: image kept (within last 2)
    assert_eq!(
        projected[2].1[1],
        Part::Image {
            mime: "image/png".to_string(),
            source: ImageSource::FileRef { artifact_id: art_3 },
        }
    );
}

#[test]
fn three_image_conversation_preserves_only_the_most_recent_image() {
    let oldest_artifact_id = artifact_id(1);
    let middle_artifact_id = artifact_id(2);
    let newest_artifact_id = artifact_id(3);
    let turns = vec![
        image_user_turn(1, "first screenshot", oldest_artifact_id),
        image_user_turn(2, "second screenshot", middle_artifact_id.clone()),
        image_user_turn(3, "third screenshot", newest_artifact_id.clone()),
    ];

    // Use the production tail (DEFAULT_TAIL_TURNS) so the test proves image
    // stripping is anchored on the latest image-bearing turn independently of
    // the protected-tail boundary used for argument truncation.
    let projected = project_for_replay(&turns, DEFAULT_TAIL_TURNS);

    // Surrounding text survives in every turn; only the image part changes.
    let stripped = Part::Text {
        text: "[image elided]".to_string(),
        synthetic: Some(true),
    };
    // Oldest image stripped (outside keep_media_turns=2 window)
    assert_eq!(
        projected[0].1,
        vec![
            Part::Text {
                text: "first screenshot".to_string(),
                synthetic: None,
            },
            stripped,
        ]
    );
    // Middle and newest images kept (within window)
    assert_eq!(
        projected[1].1,
        vec![
            Part::Text {
                text: "second screenshot".to_string(),
                synthetic: None,
            },
            Part::Image {
                mime: "image/png".to_string(),
                source: ImageSource::FileRef {
                    artifact_id: middle_artifact_id,
                },
            },
        ]
    );
    assert_eq!(
        projected[2].1,
        vec![
            Part::Text {
                text: "third screenshot".to_string(),
                synthetic: None,
            },
            Part::Image {
                mime: "image/png".to_string(),
                source: ImageSource::FileRef {
                    artifact_id: newest_artifact_id,
                },
            },
        ]
    );

    // The Anthropic request carries two image blocks (middle + newest).
    let request = AnthropicMessagesEncoder::new()
        .encode(&projected)
        .expect("projected turns should encode");
    let image_blocks = request
        .messages
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .filter(|block| block["type"] == "image")
        .count();
    assert_eq!(image_blocks, 2, "last 2 image-bearing turns should survive");
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

    let projected = project_all(&turns);

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

    let projected = project_all(&turns);
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
        Some(serde_json::json!("[read]: 12 chars."))
    );
}

#[test]
fn projected_turns_reencode_to_responses_request() {
    let call_id = tool_call_id(56);
    let turns = tool_call_then_compacted_result_turns(call_id.clone());

    let projected = project_all(&turns);
    let request = OpenAiResponsesEncoder::new()
        .encode(&projected)
        .expect("projected turns should encode");

    assert_eq!(request.input.len(), 2);
    assert_eq!(request.input[0]["type"], "function_call");
    assert_eq!(request.input[1]["type"], "function_call_output");
    assert_eq!(request.input[1]["call_id"], call_id.as_str());
    assert_eq!(request.input[1]["output"], "[read]: 12 chars.");
}

#[test]
fn projected_turns_reencode_to_anthropic_messages_request() {
    let call_id = tool_call_id(57);
    let turns = tool_call_then_compacted_result_turns(call_id.clone());

    let projected = project_all(&turns);
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
        "[read]: 12 chars."
    );
}

/// Helper: call `project_for_replay` with zero tail so all turns are eligible
/// for compaction transforms (matches the behaviour before the protected-tail
/// parameter was added).
fn project_all(turns: &[(Turn, Vec<StoredPart>)]) -> Vec<(Turn, Vec<Part>)> {
    project_for_replay(turns, 0)
}

#[test]
fn protected_tail_turns_skip_argument_truncation() {
    let long_body = "x".repeat(3_000);
    // 3 assistant turns, each with a long argument.  tail_turns = 1 means only
    // the last turn is protected.
    let turns = vec![
        turn(
            TurnRole::Assistant,
            1,
            vec![stored_part(
                part_id(1),
                Part::ToolCall {
                    id: tool_call_id(60),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"body": long_body.clone()}),
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
                Part::ToolCall {
                    id: tool_call_id(61),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"body": long_body.clone()}),
                    raw_arguments_artifact_id: None,
                },
                None,
            )],
        ),
        turn(
            TurnRole::Assistant,
            3,
            vec![stored_part(
                part_id(3),
                Part::ToolCall {
                    id: tool_call_id(62),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"body": long_body.clone()}),
                    raw_arguments_artifact_id: None,
                },
                None,
            )],
        ),
    ];

    let projected = project_for_replay(&turns, 1);

    // Turn 1 (outside tail) — truncated
    let Part::ToolCall {
        arguments: args1, ..
    } = &projected[0].1[0]
    else {
        panic!("expected tool call");
    };
    let body1 = args1["body"].as_str().unwrap();
    assert!(
        body1.ends_with("... [truncated]"),
        "old turn should be truncated"
    );
    assert!(body1.len() < 3_000);

    // Turn 2 (outside tail) — truncated
    let Part::ToolCall {
        arguments: args2, ..
    } = &projected[1].1[0]
    else {
        panic!("expected tool call");
    };
    let body2 = args2["body"].as_str().unwrap();
    assert!(
        body2.ends_with("... [truncated]"),
        "middle turn should be truncated"
    );

    // Turn 3 (inside protected tail) — NOT truncated
    let Part::ToolCall {
        arguments: args3, ..
    } = &projected[2].1[0]
    else {
        panic!("expected tool call");
    };
    let body3 = args3["body"].as_str().unwrap();
    assert_eq!(body3.len(), 3_000, "tail turn should be untouched");
    assert!(!body3.ends_with("... [truncated]"));
}

#[test]
fn default_tail_turns_is_two() {
    assert_eq!(DEFAULT_TAIL_TURNS, 2);
}

#[test]
fn fifty_kb_write_file_argument_is_truncated_outside_protected_tail() {
    let big_body = "a".repeat(50 * 1024);
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![stored_part(
            part_id(1),
            Part::ToolCall {
                id: tool_call_id(63),
                name: "write_file".to_string(),
                arguments: serde_json::json!({
                    "path": "src/main.rs",
                    "content": big_body,
                }),
                raw_arguments_artifact_id: Some(artifact_id(10)),
            },
            None,
        )],
    )];

    // With tail_turns = 0 (all turns eligible), the 50 KB content is truncated
    let projected = project_for_replay(&turns, 0);

    let Part::ToolCall {
        arguments,
        raw_arguments_artifact_id,
        ..
    } = &projected[0].1[0]
    else {
        panic!("expected tool call");
    };

    let content = arguments["content"].as_str().unwrap();
    assert!(
        content.ends_with("... [truncated]"),
        "50 KB content should be truncated"
    );
    assert!(content.len() < 50 * 1024);
    // Original bytes still retrievable via artifact
    assert!(raw_arguments_artifact_id.is_some());
}

#[test]
fn fifty_kb_write_file_argument_preserved_inside_protected_tail() {
    let big_body = "a".repeat(50 * 1024);
    let turns = vec![turn(
        TurnRole::Assistant,
        1,
        vec![stored_part(
            part_id(1),
            Part::ToolCall {
                id: tool_call_id(64),
                name: "write_file".to_string(),
                arguments: serde_json::json!({
                    "path": "src/main.rs",
                    "content": big_body.clone(),
                }),
                raw_arguments_artifact_id: Some(artifact_id(11)),
            },
            None,
        )],
    )];

    // With tail_turns = 2 (default), this single turn is in the protected tail
    let projected = project_for_replay(&turns, 2);

    let Part::ToolCall { arguments, .. } = &projected[0].1[0] else {
        panic!("expected tool call");
    };

    let content = arguments["content"].as_str().unwrap();
    assert_eq!(
        content.len(),
        50 * 1024,
        "50 KB content inside protected tail should be untouched"
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
