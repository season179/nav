//! Projection from canonical storage rows into provider replay turns.

use std::collections::{HashMap, HashSet};

use nav_types::{MessageId, PartId, ToolCallId};
use serde_json::Value;

use crate::compaction::prune::pruned_result_summary;
use crate::sessions::{Part, StoredPart, StoredTurn, Turn, TurnRole};
use crate::tools::truncation::TRUNCATED_MARKER;

const DUPLICATE_TOOL_RESULT_CONTENT: &str = "[Duplicate — see more recent result]";
const STRIPPED_IMAGE_CONTENT: &str = "[image elided]";

/// Number of trailing image-bearing user turns whose images are kept verbatim.
/// Older images are replaced with a text placeholder at replay time.
pub const KEEP_MEDIA_TURNS: usize = 2;
const MAX_ARGUMENT_STRING_CHARS: usize = 1024;

/// Default token budget for the retained tail when `keep_recent_tokens` is
/// active.  Approximate token count using a rough heuristic (chars / 4).
pub const DEFAULT_KEEP_RECENT_TOKENS: usize = 20_000;

/// Number of trailing turns whose `ToolCall.arguments` are left untouched
/// during replay projection.  Set to 2 so the user's most recent tool calls
/// stay fully visible in the context window.
pub const DEFAULT_TAIL_TURNS: usize = 2;

/// Project stored turns into replay-ready turns.
///
/// `tail_turns` controls how many trailing turns are protected from
/// argument truncation.  Pass `0` to truncate all turns (useful in tests);
/// pass [`DEFAULT_TAIL_TURNS`] for the normal production default.
///
/// Note: image stripping (replacing old images with placeholders) and
/// deduplication of tool results operate independently of `tail_turns` —
/// they apply to the entire turn history based on content, not position.
pub fn project_for_replay(turns: &[StoredTurn], tail_turns: usize) -> Vec<(Turn, Vec<Part>)> {
    let duplicate_tool_results = duplicate_tool_result_part_ids(turns);
    let tool_names = stored_tool_names_by_call_id(turns);
    let kept_image_turn_indices = kept_image_bearing_turn_indices(turns, KEEP_MEDIA_TURNS);
    let truncate_boundary = turns.len().saturating_sub(tail_turns);

    let projected = turns
        .iter()
        .enumerate()
        .map(|(index, (turn, parts))| {
            let strip_images = !kept_image_turn_indices.contains(&index);
            let truncate_arguments = index < truncate_boundary;
            (
                turn.clone(),
                parts
                    .iter()
                    .map(|part| {
                        project_stored_part(
                            part,
                            &duplicate_tool_results,
                            &tool_names,
                            strip_images,
                            truncate_arguments,
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();

    compacted_replay_window(&projected).unwrap_or(projected)
}

fn compacted_replay_window(projected: &[(Turn, Vec<Part>)]) -> Option<Vec<(Turn, Vec<Part>)>> {
    let marker_index = latest_compaction_marker_index(projected)?;
    let summary_index = marker_index.checked_add(1)?;
    let tail_start_id = compaction_tail_start_id(&projected[marker_index].1)?;

    let mut replay = Vec::new();
    replay.push(projected[marker_index].clone());
    replay.push(projected.get(summary_index)?.clone());

    let tail_start_index = match tail_start_id {
        Some(tail_start_id) => projected
            .iter()
            .position(|(turn, _)| turn.id == tail_start_id)
            .unwrap_or_else(|| summary_index.saturating_add(1)),
        None => summary_index.saturating_add(1),
    };
    replay.extend(
        projected
            .iter()
            .enumerate()
            .skip(tail_start_index)
            .filter(|(index, _)| *index != marker_index && *index != summary_index)
            .map(|(_, turn)| turn.clone()),
    );

    Some(replay)
}

fn latest_compaction_marker_index(projected: &[(Turn, Vec<Part>)]) -> Option<usize> {
    projected
        .iter()
        .rposition(|(_, parts)| has_compaction_marker(parts))
}

fn has_compaction_marker(parts: &[Part]) -> bool {
    parts
        .iter()
        .any(|part| matches!(part, Part::Compaction { .. }))
}

fn compaction_tail_start_id(parts: &[Part]) -> Option<Option<MessageId>> {
    parts.iter().find_map(|part| match part {
        Part::Compaction { tail_start_id, .. } => Some(tail_start_id.clone()),
        _ => None,
    })
}

fn duplicate_tool_result_part_ids(turns: &[StoredTurn]) -> HashSet<PartId> {
    let mut seen_hashes: HashSet<[u8; 32]> = HashSet::new();
    let mut duplicate_part_ids = HashSet::new();

    for (_, parts) in turns.iter().rev() {
        for part in parts.iter().rev() {
            if part.compacted_at.is_some() {
                continue;
            }

            let Part::ToolResult { content, .. } = &part.part else {
                continue;
            };

            let hash: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, content.as_bytes())
                .as_ref()
                .try_into()
                .expect("sha256 is always 32 bytes");

            if !seen_hashes.insert(hash) {
                duplicate_part_ids.insert(part.id.clone());
            }
        }
    }

    duplicate_part_ids
}

fn kept_image_bearing_turn_indices(turns: &[StoredTurn], keep_count: usize) -> HashSet<usize> {
    let image_indices: Vec<usize> = turns
        .iter()
        .enumerate()
        .filter(|(_, (turn, parts))| {
            turn.role == TurnRole::User
                && parts
                    .iter()
                    .any(|part| matches!(part.part, Part::Image { .. }))
        })
        .map(|(index, _)| index)
        .collect();

    let skip = image_indices.len().saturating_sub(keep_count);
    image_indices.into_iter().skip(skip).collect()
}

fn stored_tool_names_by_call_id(turns: &[StoredTurn]) -> HashMap<ToolCallId, String> {
    let mut tool_names = HashMap::new();
    for (_, parts) in turns {
        for part in parts {
            if let Part::ToolCall { id, name, .. } = &part.part {
                tool_names.insert(id.clone(), name.clone());
            }
        }
    }
    tool_names
}

fn project_stored_part(
    part: &StoredPart,
    duplicate_tool_results: &HashSet<PartId>,
    tool_names: &HashMap<ToolCallId, String>,
    strip_images: bool,
    truncate_arguments: bool,
) -> Part {
    match &part.part {
        Part::ToolResult {
            call_id,
            content,
            raw_artifact_id,
            is_error,
        } => Part::ToolResult {
            call_id: call_id.clone(),
            content: tool_result_content_for_replay(call_id, content, duplicate_tool_results, part, tool_names),
            raw_artifact_id: raw_artifact_id.clone(),
            is_error: *is_error,
        },
        Part::ToolCall {
            id,
            name,
            arguments,
            raw_arguments_artifact_id,
        } => Part::ToolCall {
            id: id.clone(),
            name: name.clone(),
            arguments: if truncate_arguments {
                truncate_argument_strings(arguments)
            } else {
                arguments.clone()
            },
            raw_arguments_artifact_id: raw_arguments_artifact_id.clone(),
        },
        Part::Image { .. } if strip_images => Part::Text {
            text: STRIPPED_IMAGE_CONTENT.to_string(),
            synthetic: Some(true),
        },
        Part::ProviderOpaque {
            kind,
            raw_artifact_id,
            ..
        } => Part::Text {
            text: format!("[Provider-specific content: {kind}; raw artifact: {raw_artifact_id}]"),
            synthetic: Some(true),
        },
        _ => part.part.clone(),
    }
}

fn tool_result_content_for_replay(
    call_id: &ToolCallId,
    content: &str,
    duplicate_tool_results: &HashSet<PartId>,
    part: &StoredPart,
    tool_names: &HashMap<ToolCallId, String>,
) -> String {
    if part.compacted_at.is_some() {
        let tool_name = tool_names.get(call_id).map(String::as_str);
        return pruned_result_summary(tool_name, content);
    }

    if duplicate_tool_results.contains(&part.id) {
        return DUPLICATE_TOOL_RESULT_CONTENT.to_string();
    }

    content.to_string()
}

fn truncate_argument_strings(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(truncate_argument_string(text)),
        Value::Array(items) => Value::Array(items.iter().map(truncate_argument_strings).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), truncate_argument_strings(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn truncate_argument_string(text: &str) -> String {
    if text.chars().count() <= MAX_ARGUMENT_STRING_CHARS {
        return text.to_string();
    }

    let marker_chars = TRUNCATED_MARKER.chars().count();
    if marker_chars >= MAX_ARGUMENT_STRING_CHARS {
        return TRUNCATED_MARKER.to_string();
    }

    let mut truncated = text
        .chars()
        .take(MAX_ARGUMENT_STRING_CHARS - marker_chars)
        .collect::<String>();
    truncated.push_str(TRUNCATED_MARKER);
    truncated
}
