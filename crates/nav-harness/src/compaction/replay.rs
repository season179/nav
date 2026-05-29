//! Projection from canonical storage rows into provider replay turns.

use std::collections::HashSet;

use nav_types::PartId;
use serde_json::Value;

use crate::sessions::{Part, StoredPart, StoredTurn, Turn, TurnRole};
use crate::tools::truncation::TRUNCATED_MARKER;

const OLD_TOOL_RESULT_CONTENT_CLEARED: &str = "[Old tool result content cleared]";
const DUPLICATE_TOOL_RESULT_CONTENT: &str = "[Duplicate — see more recent result]";
const STRIPPED_IMAGE_CONTENT: &str = "[Attached image — stripped after compression]";
const MAX_ARGUMENT_STRING_CHARS: usize = 1024;

pub fn project_for_replay(turns: &[StoredTurn]) -> Vec<(Turn, Vec<Part>)> {
    let duplicate_tool_results = duplicate_tool_result_part_ids(turns);
    let latest_image_turn_index = latest_image_bearing_user_turn_index(turns);

    turns
        .iter()
        .enumerate()
        .map(|(index, (turn, parts))| {
            let strip_images = latest_image_turn_index.is_some_and(|latest| index < latest);
            (
                turn.clone(),
                parts
                    .iter()
                    .map(|part| project_stored_part(part, &duplicate_tool_results, strip_images))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn duplicate_tool_result_part_ids(turns: &[StoredTurn]) -> HashSet<PartId> {
    let mut seen_content: HashSet<&str> = HashSet::new();
    let mut duplicate_part_ids = HashSet::new();

    for (_, parts) in turns.iter().rev() {
        for part in parts.iter().rev() {
            if part.compacted_at.is_some() {
                continue;
            }

            let Part::ToolResult { content, .. } = &part.part else {
                continue;
            };

            if !seen_content.insert(content.as_str()) {
                duplicate_part_ids.insert(part.id.clone());
            }
        }
    }

    duplicate_part_ids
}

fn latest_image_bearing_user_turn_index(turns: &[StoredTurn]) -> Option<usize> {
    turns.iter().rposition(|(turn, parts)| {
        turn.role == TurnRole::User
            && parts
                .iter()
                .any(|part| matches!(part.part, Part::Image { .. }))
    })
}

fn project_stored_part(
    part: &StoredPart,
    duplicate_tool_results: &HashSet<PartId>,
    strip_images: bool,
) -> Part {
    match &part.part {
        Part::ToolResult {
            call_id,
            content,
            raw_artifact_id,
            is_error,
        } => Part::ToolResult {
            call_id: call_id.clone(),
            content: tool_result_content_for_replay(part, content, duplicate_tool_results),
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
            arguments: truncate_argument_strings(arguments),
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
    part: &StoredPart,
    content: &str,
    duplicate_tool_results: &HashSet<PartId>,
) -> String {
    if part.compacted_at.is_some() {
        return OLD_TOOL_RESULT_CONTENT_CLEARED.to_string();
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
