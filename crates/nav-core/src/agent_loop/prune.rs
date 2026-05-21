//! Pre-call context pruning policy.
//!
//! Before each Responses request, the runner measures the assembled
//! model-visible input and sheds the oldest tool call/output pairs to fit a
//! budget. This avoids paying for a turn the provider would reject with
//! `context_length_exceeded`. The reactive recovery in [`runner`] still acts
//! as a one-shot fallback when the pre-call estimate underweights the actual
//! prompt size.

use std::collections::HashSet;

use serde_json::Value;

/// Replay budget shared by pre-call pruning and (in the future) replay
/// reconstruction. Constants live in one place so the policy stays auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayBudget {
    /// Number of trailing user-message boundaries whose tool pairs are kept
    /// verbatim. The "active turn" — i.e. tool pairs after the latest user
    /// message — is always included.
    pub raw_tool_turns: usize,
    /// Per-output cap for `function_call_output.output`. Reserved for the
    /// reducer work tracked in the context-management plan; pre-call pruning
    /// does not consult it.
    pub max_raw_tool_output_bytes: usize,
    /// Total `function_call_output.output` byte budget across the assembled
    /// input. Pre-call pruning fires when total exceeds this.
    pub max_total_tool_output_bytes: usize,
}

impl Default for ReplayBudget {
    fn default() -> Self {
        Self {
            raw_tool_turns: 2,
            max_raw_tool_output_bytes: 50 * 1024,
            max_total_tool_output_bytes: 120 * 1024,
        }
    }
}

/// Drop the oldest non-protected `function_call` + `function_call_output`
/// pairs until total tool output bytes fit under
/// [`ReplayBudget::max_total_tool_output_bytes`]. Returns the number of pairs
/// removed (`0` means no pruning was needed or every candidate was protected).
///
/// Pairs are protected when:
/// - they appear after the most recent [`ReplayBudget::raw_tool_turns`]
///   user-message boundaries, or
/// - their output text marks the tool as having failed or been blocked
///   (`tool error:` / `tool <name> blocked:` prefixes that the runner emits).
///
/// Call and output are removed together so replay stays structurally valid.
pub fn prune_to_budget(input: &mut Vec<Value>, budget: &ReplayBudget) -> usize {
    if input.is_empty() || total_tool_output_bytes(input) <= budget.max_total_tool_output_bytes {
        return 0;
    }

    let protected = protected_call_ids(input, budget.raw_tool_turns);
    let mut dropped = 0usize;
    while total_tool_output_bytes(input) > budget.max_total_tool_output_bytes {
        let Some(call_id) = oldest_droppable_call_id(input, &protected) else {
            break;
        };
        remove_pair(input, &call_id);
        dropped += 1;
    }
    dropped
}

fn total_tool_output_bytes(input: &[Value]) -> usize {
    input
        .iter()
        .filter(|item| item_type(item) == Some("function_call_output"))
        .map(|item| {
            item.get("output")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or(0)
        })
        .sum()
}

fn protected_call_ids(input: &[Value], raw_tool_turns: usize) -> HashSet<String> {
    let mut protected: HashSet<String> = HashSet::new();

    let user_indices: Vec<usize> = input
        .iter()
        .enumerate()
        .filter(|(_, item)| item_type(item) == Some("message") && role(item) == Some("user"))
        .map(|(idx, _)| idx)
        .collect();
    // Pairs after the (raw_tool_turns)-th most-recent user boundary stay
    // verbatim. With raw_tool_turns == 0 the recent-turn protection is
    // disabled (errored pairs are still protected below).
    if raw_tool_turns > 0 && !user_indices.is_empty() {
        let take = raw_tool_turns.min(user_indices.len());
        let protect_from = user_indices[user_indices.len() - take];
        for item in &input[protect_from..] {
            if item_type(item) == Some("function_call")
                && let Some(id) = call_id(item)
            {
                protected.insert(id.to_string());
            }
        }
    }

    for item in input {
        if item_type(item) == Some("function_call_output")
            && is_error_output(item)
            && let Some(id) = call_id(item)
        {
            protected.insert(id.to_string());
        }
    }
    protected
}

fn is_error_output(item: &Value) -> bool {
    let text = item.get("output").and_then(Value::as_str).unwrap_or("");
    text.starts_with("tool error:") || (text.starts_with("tool ") && text.contains(" blocked:"))
}

fn oldest_droppable_call_id(input: &[Value], protected: &HashSet<String>) -> Option<String> {
    input.iter().find_map(|item| {
        if item_type(item) != Some("function_call") {
            return None;
        }
        let id = call_id(item)?;
        if protected.contains(id) {
            return None;
        }
        Some(id.to_string())
    })
}

fn remove_pair(input: &mut Vec<Value>, call_id_str: &str) {
    // Remove the output first so the call index stays valid for the second
    // removal even if call and output sit next to each other.
    for kind in ["function_call_output", "function_call"] {
        if let Some(pos) = input
            .iter()
            .position(|item| item_type(item) == Some(kind) && call_id(item) == Some(call_id_str))
        {
            input.remove(pos);
        }
    }
}

fn item_type(item: &Value) -> Option<&str> {
    item.get("type").and_then(Value::as_str)
}

fn role(item: &Value) -> Option<&str> {
    item.get("role").and_then(Value::as_str)
}

fn call_id(item: &Value) -> Option<&str> {
    item.get("call_id").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_msg(text: &str) -> Value {
        json!({"type": "message", "role": "user", "content": text})
    }

    fn call(id: &str) -> Value {
        json!({"type": "function_call", "call_id": id, "name": "bash", "arguments": "{}"})
    }

    fn output(id: &str, body: &str) -> Value {
        json!({"type": "function_call_output", "call_id": id, "output": body})
    }

    fn budget(max_bytes: usize, raw_turns: usize) -> ReplayBudget {
        ReplayBudget {
            raw_tool_turns: raw_turns,
            max_raw_tool_output_bytes: 50 * 1024,
            max_total_tool_output_bytes: max_bytes,
        }
    }

    #[test]
    fn no_prune_when_under_budget() {
        let mut input = vec![
            user_msg("hi"),
            call("c1"),
            output("c1", "small"),
            call("c2"),
            output("c2", "also small"),
        ];
        let before = input.clone();
        let dropped = prune_to_budget(&mut input, &budget(10_000, 2));
        assert_eq!(dropped, 0);
        assert_eq!(input, before);
    }

    #[test]
    fn no_prune_when_input_empty() {
        let mut input: Vec<Value> = Vec::new();
        let dropped = prune_to_budget(&mut input, &budget(0, 0));
        assert_eq!(dropped, 0);
        assert!(input.is_empty());
    }

    #[test]
    fn drops_oldest_pair_first_when_over_budget() {
        let body = "x".repeat(100);
        let mut input = vec![
            user_msg("turn 1"),
            call("c1"),
            output("c1", &body),
            user_msg("turn 2"),
            call("c2"),
            output("c2", &body),
            user_msg("turn 3"),
            call("c3"),
            output("c3", &body),
        ];
        // raw_tool_turns = 1 protects only the most recent turn (c3). Budget
        // fits two pairs (200 bytes <= 250), so the oldest droppable (c1)
        // must be removed before c2.
        let dropped = prune_to_budget(&mut input, &budget(250, 1));
        assert_eq!(dropped, 1);
        let ids: Vec<&str> = input
            .iter()
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .collect();
        // c1 gone, c2 and c3 still present.
        assert_eq!(ids, vec!["c2", "c2", "c3", "c3"]);
    }

    #[test]
    fn removes_call_and_output_together() {
        let body = "x".repeat(200);
        let mut input = vec![
            user_msg("turn 1"),
            call("c1"),
            output("c1", &body),
            user_msg("turn 2"),
            call("c2"),
            output("c2", &body),
        ];
        let dropped = prune_to_budget(&mut input, &budget(150, 1));
        assert_eq!(dropped, 1);
        // Pair preservation: no `function_call_output` without a matching
        // `function_call` (and vice versa) after pruning.
        let mut calls: Vec<&str> = input
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .collect();
        let mut outputs: Vec<&str> = input
            .iter()
            .filter(|item| {
                item.get("type").and_then(Value::as_str) == Some("function_call_output")
            })
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .collect();
        calls.sort();
        outputs.sort();
        assert_eq!(calls, outputs, "call and output ids must stay paired");
    }

    #[test]
    fn protects_recent_user_turns() {
        let body = "x".repeat(100);
        let mut input = vec![
            user_msg("old"),
            call("c1"),
            output("c1", &body),
            user_msg("mid"),
            call("c2"),
            output("c2", &body),
            user_msg("recent"),
            call("c3"),
            output("c3", &body),
        ];
        // raw_tool_turns = 2 protects "mid" and "recent" turns. Even with an
        // impossibly small budget, c2 and c3 must survive.
        let dropped = prune_to_budget(&mut input, &budget(0, 2));
        assert_eq!(dropped, 1);
        let surviving_call_ids: Vec<&str> = input
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .collect();
        assert_eq!(surviving_call_ids, vec!["c2", "c3"]);
    }

    #[test]
    fn protects_failed_tool_outputs() {
        let body = "x".repeat(200);
        let mut input = vec![
            user_msg("old turn"),
            call("c1"),
            output("c1", "tool error: bash exited with status 1"),
            call("c2"),
            output("c2", &body),
            user_msg("recent turn"),
            call("c3"),
            output("c3", &body),
        ];
        // Budget tight enough that without protection, both c1 and c2 would
        // need to go. raw_tool_turns = 1 protects only c3; c1 must still
        // survive because its output is an error.
        let dropped = prune_to_budget(&mut input, &budget(150, 1));
        assert_eq!(dropped, 1);
        let surviving_call_ids: Vec<&str> = input
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .collect();
        assert_eq!(surviving_call_ids, vec!["c1", "c3"]);
    }

    #[test]
    fn protects_blocked_tool_outputs() {
        let body = "x".repeat(200);
        let mut input = vec![
            user_msg("old turn"),
            call("c1"),
            output("c1", "tool bash blocked: not in allowlist"),
            user_msg("recent turn"),
            call("c2"),
            output("c2", &body),
        ];
        let dropped = prune_to_budget(&mut input, &budget(150, 1));
        assert_eq!(dropped, 0);
        assert!(
            input
                .iter()
                .any(|item| item.get("call_id").and_then(Value::as_str) == Some("c1"))
        );
    }

    #[test]
    fn returns_zero_when_only_protected_pairs_remain() {
        let body = "x".repeat(500);
        let mut input = vec![user_msg("only turn"), call("c1"), output("c1", &body)];
        let before = input.clone();
        let dropped = prune_to_budget(&mut input, &budget(10, 1));
        assert_eq!(dropped, 0);
        assert_eq!(input, before);
    }

    #[test]
    fn drops_multiple_pairs_until_under_budget() {
        let body = "x".repeat(100);
        let mut input = vec![
            user_msg("turn"),
            call("c1"),
            output("c1", &body),
            call("c2"),
            output("c2", &body),
            call("c3"),
            output("c3", &body),
            user_msg("recent"),
            call("c4"),
            output("c4", &body),
        ];
        // Protect only the recent turn (c4); budget must shed at least c1 and
        // c2 to fit (c3 + c4 = 200 bytes).
        let dropped = prune_to_budget(&mut input, &budget(250, 1));
        assert_eq!(dropped, 2);
        let surviving_call_ids: Vec<&str> = input
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .collect();
        assert_eq!(surviving_call_ids, vec!["c3", "c4"]);
    }

    #[test]
    fn handles_orphan_call_without_matching_output() {
        // A `function_call` with no `function_call_output` (e.g. mid-stream
        // before the output is appended) is still droppable; remove_pair
        // tolerates the missing output.
        let body = "x".repeat(200);
        let mut input = vec![
            user_msg("turn"),
            call("c1"),
            output("c1", &body),
            user_msg("recent"),
            call("c2"),
        ];
        let dropped = prune_to_budget(&mut input, &budget(150, 1));
        assert_eq!(dropped, 1);
        let remaining_call_ids: Vec<&str> = input
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .collect();
        assert_eq!(remaining_call_ids, vec!["c2"]);
    }
}
