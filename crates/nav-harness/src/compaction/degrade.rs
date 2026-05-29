//! Encode-time degraded replay for unmappable old turns (ENC-10).
//!
//! When the canonical history is replayed into a target dialect that cannot
//! represent some parts, this pre-pass applies the plan-defined fallbacks
//! before the dialect encoder runs:
//!
//! 1. **Drop** `Thinking` parts for dialects without thinking support.
//! 2. **Degrade** unpaired tool activity (a `ToolCall` or `ToolResult` whose
//!    partner is missing) into plain text, so dialects that require strict
//!    tool-call pairing still accept the request.
//!
//! The escalating last resort — replacing the head with a compaction summary —
//! lives in the session store, not here. Each fallback is recorded in
//! [`DegradeOutcome::events`] so callers can surface why history shrank.

use std::collections::HashSet;

use nav_types::{MessageId, ToolCallId};

use crate::models::ApiKind;
use crate::sessions::{Part, Turn};

/// Capabilities of a target dialect that decide which fallbacks apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialectCaps {
    /// Whether the dialect can carry `Thinking` parts at all.
    pub supports_thinking: bool,
}

impl DialectCaps {
    /// Capabilities for a concrete provider API shape.
    ///
    /// `supports_thinking` tracks whether the matching encoder can carry a
    /// `Thinking` part onto the wire at all, so it stays in lockstep with the
    /// encoders (the source of truth):
    /// - Anthropic Messages emits `thinking` content blocks.
    /// - OpenAI Responses emits an encrypted `reasoning` item (the encoder
    ///   keeps encrypted thinking and drops the rest), so it *can* carry
    ///   thinking — `true`.
    /// - Chat Completions and the ChatGPT subscription shape have no thinking
    ///   representation; their encoders silently drop it — `false`, so we drop
    ///   (and report) it here instead.
    pub fn for_api_kind(api_kind: ApiKind) -> Self {
        match api_kind {
            ApiKind::AnthropicMessages | ApiKind::OpenAiResponses => Self {
                supports_thinking: true,
            },
            ApiKind::OpenAiCompletions | ApiKind::ChatGptSubscription => Self {
                supports_thinking: false,
            },
        }
    }
}

/// A single fallback applied while degrading the replay history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackEvent {
    /// A `Thinking` part was dropped because the target dialect cannot carry it.
    DroppedThinking { turn_id: MessageId },
    /// An unpaired tool call/result was rendered as plain text so a dialect
    /// that requires strict tool-call pairing still accepts the request.
    DegradedToolActivity {
        turn_id: MessageId,
        call_id: ToolCallId,
    },
}

/// The degraded replay history plus the fallbacks that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct DegradeOutcome {
    pub turns: Vec<(Turn, Vec<Part>)>,
    pub events: Vec<FallbackEvent>,
}

/// Apply the plan-defined fallbacks for parts the target dialect cannot map.
pub fn degrade_for_dialect(turns: Vec<(Turn, Vec<Part>)>, caps: DialectCaps) -> DegradeOutcome {
    let paired_calls = paired_tool_call_ids(&turns);
    let mut events = Vec::new();

    let turns = turns
        .into_iter()
        .map(|(turn, parts)| {
            let parts = parts
                .into_iter()
                .filter_map(|part| degrade_part(&turn.id, part, caps, &paired_calls, &mut events))
                .collect();
            (turn, parts)
        })
        .collect();

    DegradeOutcome { turns, events }
}

/// Tool call ids that have both a call and a result somewhere in the history.
/// Anything outside this set is orphaned and must be degraded to text.
fn paired_tool_call_ids(turns: &[(Turn, Vec<Part>)]) -> HashSet<ToolCallId> {
    let mut call_ids = HashSet::new();
    let mut result_ids = HashSet::new();

    for part in turns.iter().flat_map(|(_, parts)| parts) {
        match part {
            Part::ToolCall { id, .. } => {
                call_ids.insert(id.clone());
            }
            Part::ToolResult { call_id, .. } => {
                result_ids.insert(call_id.clone());
            }
            _ => {}
        }
    }

    call_ids.intersection(&result_ids).cloned().collect()
}

fn degrade_part(
    turn_id: &MessageId,
    part: Part,
    caps: DialectCaps,
    paired_calls: &HashSet<ToolCallId>,
    events: &mut Vec<FallbackEvent>,
) -> Option<Part> {
    match part {
        Part::Thinking { .. } if !caps.supports_thinking => {
            events.push(FallbackEvent::DroppedThinking {
                turn_id: turn_id.clone(),
            });
            None
        }
        Part::ToolCall {
            id,
            name,
            arguments,
            ..
        } if !paired_calls.contains(&id) => {
            events.push(FallbackEvent::DegradedToolActivity {
                turn_id: turn_id.clone(),
                call_id: id,
            });
            Some(synthetic_text(format!("[Tool call: {name}({arguments})]")))
        }
        Part::ToolResult {
            call_id, content, ..
        } if !paired_calls.contains(&call_id) => {
            events.push(FallbackEvent::DegradedToolActivity {
                turn_id: turn_id.clone(),
                call_id,
            });
            Some(synthetic_text(format!("[Tool result: {content}]")))
        }
        other => Some(other),
    }
}

fn synthetic_text(text: String) -> Part {
    Part::Text {
        text,
        synthetic: Some(true),
    }
}
