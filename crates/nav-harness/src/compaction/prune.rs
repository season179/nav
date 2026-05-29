//! Cheap tool-result pruning for model-visible replay.

use std::collections::HashMap;

use nav_types::{PartId, ToolCallId};

use crate::sessions::{ModelTurn, Part, StoredTurn, TurnPart};

pub const PRUNE_PROTECT_TOKENS: usize = 40_000;
pub const OLD_TOOL_RESULT_CONTENT_CLEARED: &str = "[Old tool result content cleared]";

const APPROX_CHARS_PER_TOKEN: usize = 4;
const PROTECTED_TOOL_NAMES: &[&str] = &["skill"];

#[derive(Debug)]
struct ToolResultCandidate {
    part_id: PartId,
    tokens: usize,
    turn_created_at: i64,
    part_created_at: i64,
}

#[derive(Debug)]
struct ModelToolResultCandidate {
    turn_index: usize,
    part_index: usize,
    tokens: usize,
}

pub fn tool_result_part_ids_to_prune(turns: &[StoredTurn]) -> Vec<PartId> {
    let tool_names = tool_names_by_call_id(turns);
    let mut candidates = tool_result_candidates(turns, &tool_names);
    candidates.sort_by(|left, right| {
        right
            .turn_created_at
            .cmp(&left.turn_created_at)
            .then_with(|| right.part_created_at.cmp(&left.part_created_at))
            .then_with(|| right.part_id.as_str().cmp(left.part_id.as_str()))
    });

    let mut visible_tokens: usize = 0;
    let mut prune = Vec::new();

    for candidate in candidates {
        let next_visible_tokens = visible_tokens.saturating_add(candidate.tokens);
        if next_visible_tokens <= PRUNE_PROTECT_TOKENS {
            visible_tokens = next_visible_tokens;
        } else {
            prune.push(candidate.part_id);
        }
    }

    prune
}

pub fn project_model_turns_for_tool_result_pruning(turns: &mut [ModelTurn]) {
    let tool_names = model_tool_names_by_call_id(turns);
    let mut candidates = model_tool_result_candidates(turns, &tool_names);
    candidates.sort_by(|left, right| {
        right
            .turn_index
            .cmp(&left.turn_index)
            .then_with(|| right.part_index.cmp(&left.part_index))
    });

    let mut visible_tokens: usize = 0;
    let mut pruned_positions = Vec::new();

    for candidate in candidates {
        let next_visible_tokens = visible_tokens.saturating_add(candidate.tokens);
        if next_visible_tokens <= PRUNE_PROTECT_TOKENS {
            visible_tokens = next_visible_tokens;
        } else {
            pruned_positions.push((candidate.turn_index, candidate.part_index));
        }
    }

    for (turn_index, part_index) in pruned_positions {
        if let TurnPart::ToolResult { content, .. } = &mut turns[turn_index].parts[part_index] {
            *content = OLD_TOOL_RESULT_CONTENT_CLEARED.to_string();
        }
    }
}

fn tool_names_by_call_id(turns: &[StoredTurn]) -> HashMap<ToolCallId, &str> {
    let mut tool_names = HashMap::new();

    for (_, parts) in turns {
        for part in parts {
            if let Part::ToolCall { id, name, .. } = &part.part {
                tool_names.insert(id.clone(), name.as_str());
            }
        }
    }

    tool_names
}

fn tool_result_candidates(
    turns: &[StoredTurn],
    tool_names: &HashMap<ToolCallId, &str>,
) -> Vec<ToolResultCandidate> {
    let mut candidates = Vec::new();

    for (turn, parts) in turns {
        for part in parts {
            if part.compacted_at.is_some() {
                continue;
            }

            let Part::ToolResult {
                call_id, content, ..
            } = &part.part
            else {
                continue;
            };

            if tool_names
                .get(call_id)
                .is_some_and(|name| PROTECTED_TOOL_NAMES.contains(name))
            {
                continue;
            }

            candidates.push(ToolResultCandidate {
                part_id: part.id.clone(),
                tokens: approximate_tokens(content),
                turn_created_at: turn.created_at,
                part_created_at: part.created_at,
            });
        }
    }

    candidates
}

fn model_tool_names_by_call_id(turns: &[ModelTurn]) -> HashMap<String, String> {
    let mut tool_names = HashMap::new();

    for turn in turns {
        for part in &turn.parts {
            if let TurnPart::ToolCall(tool_call) = part {
                tool_names.insert(tool_call.id.clone(), tool_call.name.clone());
                if let Some(tool_call_id) = &tool_call.tool_call_id {
                    tool_names.insert(tool_call_id.to_string(), tool_call.name.clone());
                }
            }
        }
    }

    tool_names
}

fn model_tool_result_candidates(
    turns: &[ModelTurn],
    tool_names: &HashMap<String, String>,
) -> Vec<ModelToolResultCandidate> {
    let mut candidates = Vec::new();

    for (turn_index, turn) in turns.iter().enumerate() {
        for (part_index, part) in turn.parts.iter().enumerate() {
            let TurnPart::ToolResult {
                tool_call_id,
                content,
            } = part
            else {
                continue;
            };

            if content == OLD_TOOL_RESULT_CONTENT_CLEARED {
                continue;
            }

            if tool_names
                .get(tool_call_id)
                .is_some_and(|name| PROTECTED_TOOL_NAMES.contains(&name.as_str()))
            {
                continue;
            }

            candidates.push(ModelToolResultCandidate {
                turn_index,
                part_index,
                tokens: approximate_tokens(content),
            });
        }
    }

    candidates
}

fn approximate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    text.chars().count().div_ceil(APPROX_CHARS_PER_TOKEN)
}
