//! ENC-10: degraded replay for unmappable old turns.
//!
//! When the canonical history is replayed into a target dialect that cannot
//! represent some parts (thinking traces, orphaned tool activity), an
//! encode-time pre-pass applies the plan-defined fallbacks (drop / degrade)
//! and reports each one so callers can explain why history shrank.

use nav_harness::compaction::degrade::{
    DegradeOutcome, DialectCaps, FallbackEvent, degrade_for_dialect,
};
use nav_harness::models::{ApiKind, OpenAiChatCompletionsEncoder};
use nav_harness::sessions::{Part, Turn, TurnMeta, TurnRole};
use nav_types::{MessageId, RunId, ToolCallId};

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test message id should be UUIDv7-shaped")
}

fn run_id(suffix: u64) -> RunId {
    RunId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test run id should be UUIDv7-shaped")
}

fn tool_call_id(suffix: u64) -> ToolCallId {
    ToolCallId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}"))
        .expect("test tool call id should be UUIDv7-shaped")
}

fn turn(suffix: u64, role: TurnRole, parts: Vec<Part>) -> (Turn, Vec<Part>) {
    (
        Turn {
            id: message_id(suffix),
            run_id: run_id(1),
            seq: suffix as u32,
            role,
            meta: TurnMeta::default(),
            created_at: 1_700_000_000_000 + suffix as i64,
        },
        parts,
    )
}

fn text(value: &str) -> Part {
    Part::Text {
        text: value.to_string(),
        synthetic: None,
    }
}

#[test]
fn non_thinking_dialect_drops_thinking_and_reports_it() {
    let turns = vec![
        turn(0x10, TurnRole::User, vec![text("why 42?")]),
        turn(
            0x11,
            TurnRole::Assistant,
            vec![
                Part::Thinking {
                    text: "SECRET_REASONING".to_string(),
                    provider_hint: None,
                },
                text("The answer is 42."),
            ],
        ),
    ];

    let DegradeOutcome { turns, events } =
        degrade_for_dialect(turns, DialectCaps::for_api_kind(ApiKind::OpenAiCompletions));

    let remaining: Vec<&Part> = turns.iter().flat_map(|(_, parts)| parts).collect();
    assert!(
        !remaining
            .iter()
            .any(|part| matches!(part, Part::Thinking { .. })),
        "thinking parts must be dropped for a non-thinking dialect"
    );
    assert!(
        remaining
            .iter()
            .any(|part| matches!(part, Part::Text { text, .. } if text == "The answer is 42.")),
        "non-thinking text must survive"
    );
    assert_eq!(
        events,
        vec![FallbackEvent::DroppedThinking {
            turn_id: message_id(0x11),
        }],
    );
}

#[test]
fn dialect_capabilities_track_thinking_support() {
    assert!(DialectCaps::for_api_kind(ApiKind::AnthropicMessages).supports_thinking);
    assert!(DialectCaps::for_api_kind(ApiKind::OpenAiResponses).supports_thinking);
    assert!(!DialectCaps::for_api_kind(ApiKind::OpenAiCompletions).supports_thinking);
    assert!(!DialectCaps::for_api_kind(ApiKind::ChatGptSubscription).supports_thinking);
}

fn tool_call(suffix: u64, name: &str, arguments: serde_json::Value) -> Part {
    Part::ToolCall {
        id: tool_call_id(suffix),
        name: name.to_string(),
        arguments,
        raw_arguments_artifact_id: None,
    }
}

fn tool_result(suffix: u64, content: &str) -> Part {
    Part::ToolResult {
        call_id: tool_call_id(suffix),
        content: content.to_string(),
        raw_artifact_id: None,
        is_error: false,
    }
}

#[test]
fn orphaned_tool_call_degrades_to_text_and_reports_it() {
    // The result was never recorded (interrupted run), leaving a dangling call.
    let turns = vec![turn(
        0x31,
        TurnRole::Assistant,
        vec![tool_call(
            0x88,
            "read_file",
            serde_json::json!({ "path": "src/main.rs" }),
        )],
    )];

    let DegradeOutcome { turns, events } =
        degrade_for_dialect(turns, DialectCaps::for_api_kind(ApiKind::AnthropicMessages));

    let parts: Vec<&Part> = turns.iter().flat_map(|(_, parts)| parts).collect();
    assert!(
        !parts
            .iter()
            .any(|part| matches!(part, Part::ToolCall { .. })),
        "unpaired tool call must be degraded away"
    );
    assert!(
        parts.iter().any(|part| matches!(
            part,
            Part::Text { text, synthetic: Some(true) } if text.contains("read_file")
        )),
        "degraded text must name the called tool"
    );
    assert_eq!(
        events,
        vec![FallbackEvent::DegradedToolActivity {
            turn_id: message_id(0x31),
            call_id: tool_call_id(0x88),
        }],
    );
}

#[test]
fn paired_tool_activity_is_left_intact() {
    let turns = vec![
        turn(
            0x41,
            TurnRole::Assistant,
            vec![tool_call(0x77, "read_file", serde_json::json!({}))],
        ),
        turn(0x42, TurnRole::Assistant, vec![tool_result(0x77, "ok")]),
    ];

    let DegradeOutcome { turns, events } =
        degrade_for_dialect(turns, DialectCaps::for_api_kind(ApiKind::AnthropicMessages));

    let parts: Vec<&Part> = turns.iter().flat_map(|(_, parts)| parts).collect();
    assert!(parts.iter().any(|p| matches!(p, Part::ToolCall { .. })));
    assert!(parts.iter().any(|p| matches!(p, Part::ToolResult { .. })));
    assert!(
        events.is_empty(),
        "paired tool activity needs no fallback: {events:?}"
    );
}

#[test]
fn thinking_capable_dialect_keeps_thinking_without_events() {
    let turns = vec![turn(
        0x11,
        TurnRole::Assistant,
        vec![
            Part::Thinking {
                text: "SECRET_REASONING".to_string(),
                provider_hint: None,
            },
            text("The answer is 42."),
        ],
    )];

    let DegradeOutcome { turns, events } =
        degrade_for_dialect(turns, DialectCaps::for_api_kind(ApiKind::AnthropicMessages));

    assert!(
        turns
            .iter()
            .flat_map(|(_, parts)| parts)
            .any(|part| matches!(part, Part::Thinking { .. })),
        "thinking-capable dialect must retain thinking"
    );
    assert!(events.is_empty(), "no fallbacks expected: {events:?}");
}

#[test]
fn orphaned_tool_result_degrades_to_text_and_reports_it() {
    // The matching tool call lives in a head turn that compaction dropped, so
    // only the result survives — strict dialects would reject the unpaired part.
    let turns = vec![turn(
        0x21,
        TurnRole::Assistant,
        vec![Part::ToolResult {
            call_id: tool_call_id(0x99),
            content: "exit code 0, build succeeded".to_string(),
            raw_artifact_id: None,
            is_error: false,
        }],
    )];

    let DegradeOutcome { turns, events } =
        degrade_for_dialect(turns, DialectCaps::for_api_kind(ApiKind::AnthropicMessages));

    let parts: Vec<&Part> = turns.iter().flat_map(|(_, parts)| parts).collect();
    assert!(
        !parts
            .iter()
            .any(|part| matches!(part, Part::ToolResult { .. })),
        "unpaired tool result must be degraded away"
    );
    assert!(
        parts.iter().any(|part| matches!(
            part,
            Part::Text { text, synthetic: Some(true) } if text.contains("build succeeded")
        )),
        "degraded text must preserve the tool result content as synthetic text"
    );
    assert_eq!(
        events,
        vec![FallbackEvent::DegradedToolActivity {
            turn_id: message_id(0x21),
            call_id: tool_call_id(0x99),
        }],
    );
}

/// Acceptance: switching from a thinking-capable provider (Anthropic) to one
/// without (Chat Completions) drops the reasoning trace, but the next reply
/// still grounds in the prior conversation.
#[test]
fn swap_to_non_thinking_provider_still_grounds_in_prior_conversation() {
    let history = vec![
        turn(
            0x10,
            TurnRole::User,
            vec![text("Remember the passphrase: BLUE_OTTER.")],
        ),
        turn(
            0x11,
            TurnRole::Assistant,
            vec![
                Part::Thinking {
                    text: "SECRET_REASONING_TRACE".to_string(),
                    provider_hint: None,
                },
                text("Got it, the passphrase is BLUE_OTTER."),
            ],
        ),
        turn(0x20, TurnRole::User, vec![text("What was the passphrase?")]),
    ];

    let DegradeOutcome { turns, events } = degrade_for_dialect(
        history,
        DialectCaps::for_api_kind(ApiKind::OpenAiCompletions),
    );

    assert_eq!(
        events,
        vec![FallbackEvent::DroppedThinking {
            turn_id: message_id(0x11),
        }],
    );

    let request = OpenAiChatCompletionsEncoder::new()
        .encode(&turns)
        .expect("degraded history should encode for chat completions");
    let wire = request
        .messages
        .iter()
        .filter_map(|message| message.content.as_ref().map(ToString::to_string))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !wire.contains("SECRET_REASONING_TRACE"),
        "thinking trace must not survive into a non-thinking dialect"
    );
    assert!(
        wire.contains("BLUE_OTTER"),
        "the model must still see the grounding established earlier: {wire}"
    );
    assert!(
        wire.contains("What was the passphrase?"),
        "the latest user question must be present"
    );
}

/// An assistant turn whose only content is a dropped `Thinking` part is left
/// empty by the degrade pass. The encoder must skip it rather than emit a
/// content-less message that a provider would reject.
#[test]
fn turn_emptied_by_thinking_drop_produces_no_dangling_message() {
    let history = vec![
        turn(0x10, TurnRole::User, vec![text("hi")]),
        turn(
            0x11,
            TurnRole::Assistant,
            vec![Part::Thinking {
                text: "only reasoning, no visible answer".to_string(),
                provider_hint: None,
            }],
        ),
    ];

    let DegradeOutcome { turns, .. } = degrade_for_dialect(
        history,
        DialectCaps::for_api_kind(ApiKind::OpenAiCompletions),
    );

    let request = OpenAiChatCompletionsEncoder::new()
        .encode(&turns)
        .expect("degraded history should encode for chat completions");

    // Only the user turn survives into the wire request; the emptied assistant
    // turn contributes no message.
    assert_eq!(request.messages.len(), 1);
}
