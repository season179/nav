//! Fixture-driven tests for the Anthropic Messages canonical encoder.

use nav_harness::models::{
    AnthropicMessagesDecodeInput, AnthropicMessagesDecoder, AnthropicMessagesEncoder,
    AnthropicToolDefinition, ApiKind, Decoder, Encoder,
};
use nav_harness::sessions::{ImageSource, ModelTurn, Part, ToolCall, Turn, TurnMeta, TurnRole};
use nav_types::{ArtifactId, MessageId, ProviderPayloadId, RunId, ToolCallId};

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
            signature: None,
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

#[test]
fn anthropic_thinking_signature_round_trips() {
    // Decode an Anthropic response whose thinking block carries a signature, then
    // re-encode the canonical parts and assert the signature survives verbatim.
    let decoded = AnthropicMessagesDecoder::new()
        .decode(&AnthropicMessagesDecodeInput {
            provider_payload_id: ProviderPayloadId::try_new(
                "pay_0000018bcfe56800_0000000000000001",
            )
            .expect("test provider payload id"),
            raw_artifact_id: ArtifactId::try_new("art_0000018bcfe56800_0000000000000001")
                .expect("test artifact id"),
            run_id: run_id(1),
            provider_id: Some("anthropic".to_string()),
            raw_json: include_bytes!("fixtures/anthropic-messages/thinking.json").to_vec(),
            created_at: 1_700_000_000_000,
        })
        .expect("fixture should decode");

    let signature = decoded.turns[0]
        .parts
        .iter()
        .find_map(|part| match &part.part {
            Part::Thinking { signature, .. } => Some(signature.clone()),
            _ => None,
        })
        .expect("a thinking part should be decoded");
    assert_eq!(signature.as_deref(), Some("sig_01thinking"));

    let parts: Vec<Part> = decoded.turns[0]
        .parts
        .iter()
        .map(|part| part.part.clone())
        .collect();
    let turns = vec![turn(TurnRole::Assistant, 1, parts)];

    let request = AnthropicMessagesEncoder::new()
        .encode(&turns)
        .expect("encoding should succeed");

    let thinking_block = request.messages[0]["content"]
        .as_array()
        .expect("assistant content array")
        .iter()
        .find(|block| block["type"] == "thinking")
        .expect("a re-emitted thinking block");
    assert_eq!(
        thinking_block["thinking"],
        "I should inspect the requested file first."
    );
    assert_eq!(thinking_block["signature"], "sig_01thinking");
}

// ---------------------------------------------------------------------------
// cache_control breakpoints (plans/context-management.md §2.4)
// ---------------------------------------------------------------------------

fn tool(name: &str) -> AnthropicToolDefinition {
    AnthropicToolDefinition {
        name: name.to_string(),
        description: format!("{name} tool"),
        input_schema: serde_json::json!({ "type": "object" }),
    }
}

fn ephemeral() -> serde_json::Value {
    serde_json::json!({ "type": "ephemeral" })
}

#[test]
fn cache_control_marks_only_the_last_tool_definition() {
    let request = <AnthropicMessagesEncoder as Encoder>::encode(
        &AnthropicMessagesEncoder::new().with_tools(vec![tool("bash"), tool("edit"), tool("read")]),
        &[ModelTurn::user_text("hi")],
    )
    .expect("encoding should succeed");

    let body = request.to_request_body();
    let tools = body["tools"].as_array().expect("tools array");

    assert_eq!(tools.len(), 3);
    assert!(tools[0].get("cache_control").is_none());
    assert!(tools[1].get("cache_control").is_none());
    assert_eq!(tools[2]["cache_control"], ephemeral());
}

#[test]
fn cache_control_marks_the_static_system_block_and_drops_boundary_markers() {
    use nav_harness::context::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY as B;

    let system = format!("STATIC{B}SEMI-STATIC{B}VOLATILE");
    let request = AnthropicMessagesEncoder::new()
        .with_system(system)
        .encode(&[])
        .expect("encoding should succeed");

    let body = request.to_request_body();
    let blocks = body["system"].as_array().expect("system array");

    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[0]["text"], "STATIC");
    assert_eq!(blocks[0]["cache_control"], ephemeral());
    assert_eq!(blocks[1]["text"], "SEMI-STATIC");
    assert!(blocks[1].get("cache_control").is_none());
    assert_eq!(blocks[2]["text"], "VOLATILE");
    assert!(blocks[2].get("cache_control").is_none());

    // The boundary sentinel must never reach the model.
    assert!(!body.to_string().contains("DYNAMIC_BOUNDARY"));
}

/// `cache_control` on a message attaches to the last block of its `content`.
fn message_cache_control(message: &serde_json::Value) -> Option<&serde_json::Value> {
    message["content"]
        .as_array()
        .and_then(|content| content.last())
        .and_then(|block| block.get("cache_control"))
}

#[test]
fn cache_control_rolls_over_the_last_two_messages() {
    let request = <AnthropicMessagesEncoder as Encoder>::encode(
        &AnthropicMessagesEncoder::new(),
        &[
            ModelTurn::user_text("one"),
            ModelTurn::assistant_text("two"),
            ModelTurn::user_text("three"),
        ],
    )
    .expect("encoding should succeed");

    let body = request.to_request_body();
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 3);
    assert!(message_cache_control(&messages[0]).is_none());
    assert_eq!(message_cache_control(&messages[1]), Some(&ephemeral()));
    assert_eq!(message_cache_control(&messages[2]), Some(&ephemeral()));
}

#[test]
fn cache_control_survives_a_turn_with_more_than_twenty_tool_result_blocks() {
    // A single agentic turn can append far more than 20 tool-result blocks; the
    // marker is message-level so it still lands on the message's last block.
    let bulky: Vec<serde_json::Value> = (0..25)
        .map(|i| {
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": format!("call-{i}"),
                "content": "ok"
            })
        })
        .collect();
    let messages = vec![
        serde_json::json!({ "role": "assistant", "content": [{ "type": "text", "text": "earlier" }] }),
        serde_json::json!({ "role": "user", "content": bulky }),
    ];

    let body = nav_harness::models::AnthropicMessagesRequest::new(messages).to_request_body();
    let messages = body["messages"].as_array().expect("messages array");

    // Both the second-to-last and the last message carry the breakpoint, and on
    // the last message it sits on the 25th (final) content block — not block 20.
    assert_eq!(message_cache_control(&messages[0]), Some(&ephemeral()));
    let last_content = messages[1]["content"].as_array().unwrap();
    assert_eq!(last_content.len(), 25);
    assert_eq!(last_content[24]["cache_control"], ephemeral());
    assert!(
        last_content[..24]
            .iter()
            .all(|block| block.get("cache_control").is_none())
    );
}

#[test]
fn subagent_fork_shifts_the_rolling_pair_one_message_earlier() {
    // The fork's throwaway last message must stay out of the shared cache, so
    // the pair lands on the two messages *before* the tail.
    let request = <AnthropicMessagesEncoder as Encoder>::encode(
        &AnthropicMessagesEncoder::new().subagent_fork(true),
        &[
            ModelTurn::user_text("one"),
            ModelTurn::assistant_text("two"),
            ModelTurn::user_text("three"),
            ModelTurn::assistant_text("four"),
        ],
    )
    .expect("encoding should succeed");

    let body = request.to_request_body();
    let messages = body["messages"].as_array().expect("messages array");

    assert_eq!(messages.len(), 4);
    assert!(message_cache_control(&messages[0]).is_none());
    assert_eq!(message_cache_control(&messages[1]), Some(&ephemeral()));
    assert_eq!(message_cache_control(&messages[2]), Some(&ephemeral()));
    assert!(message_cache_control(&messages[3]).is_none());
}

fn count_cache_control(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) => {
            let here = usize::from(map.contains_key("cache_control"));
            here + map.values().map(count_cache_control).sum::<usize>()
        }
        serde_json::Value::Array(items) => items.iter().map(count_cache_control).sum(),
        _ => 0,
    }
}

#[test]
fn cache_control_uses_at_most_four_breakpoints() {
    use nav_harness::context::system_prompt::SYSTEM_PROMPT_DYNAMIC_BOUNDARY as B;

    // A full request — system (3 blocks), tools, and many messages — exercises
    // every breakpoint: tools-end, static-system-end, and the rolling pair.
    let request = <AnthropicMessagesEncoder as Encoder>::encode(
        &AnthropicMessagesEncoder::new()
            .with_system(format!("STATIC{B}SEMI{B}VOLATILE"))
            .with_tools(vec![tool("bash"), tool("edit"), tool("read")]),
        &[
            ModelTurn::user_text("one"),
            ModelTurn::assistant_text("two"),
            ModelTurn::user_text("three"),
            ModelTurn::assistant_text("four"),
            ModelTurn::user_text("five"),
        ],
    )
    .expect("encoding should succeed");

    let body = request.to_request_body();

    // Anthropic caps requests at 4 cache_control breakpoints.
    assert_eq!(count_cache_control(&body), 4);
}

#[test]
fn body_omits_tools_when_there_are_none() {
    // An empty `tools: []` is noise; the body should omit the key entirely
    // (matching the OpenAI completions request builder).
    let request = AnthropicMessagesEncoder::new()
        .encode(&[turn(
            TurnRole::User,
            1,
            vec![Part::Text {
                text: "hi".to_string(),
                synthetic: None,
            }],
        )])
        .expect("encoding should succeed");

    let body = request.to_request_body();

    assert!(body.get("tools").is_none());
}

#[test]
fn body_omits_system_when_unset() {
    let request = AnthropicMessagesEncoder::new()
        .encode(&[turn(
            TurnRole::User,
            1,
            vec![Part::Text {
                text: "hi".to_string(),
                synthetic: None,
            }],
        )])
        .expect("encoding should succeed");

    let body = request.to_request_body();

    assert!(body.get("system").is_none());
}

#[test]
fn cache_control_marks_a_system_prompt_with_no_boundary() {
    // A system prompt that carries no boundary sentinel is wholly static, so the
    // single block still gets the static-system-end breakpoint.
    let request = AnthropicMessagesEncoder::new()
        .with_system("All static.")
        .encode(&[])
        .expect("encoding should succeed");

    let body = request.to_request_body();
    let blocks = body["system"].as_array().expect("system array");

    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["text"], "All static.");
    assert_eq!(blocks[0]["cache_control"], ephemeral());
}
