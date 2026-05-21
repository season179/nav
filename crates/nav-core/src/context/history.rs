//! Model-visible prompt history invariants.
//!
//! Helpers used by both the live agent loop and replay reconstruction so the
//! two paths enforce the same wire-format rules: function calls and
//! function-call outputs are paired, orphan items are removed or repaired,
//! unsupported modalities (e.g. images on a text-only model) are stripped,
//! and the replay output budget is applied uniformly. Category accounting for
//! `/context` lives in [`super::report`], which already groups the assembled
//! input into labeled buckets for rendering.
//!
//! Both call sites own a `Vec<Value>` shaped like the Responses API `input`
//! array. The seam is function-based: `replay.rs` calls the orchestrating
//! `normalize_for_request` after walking the durable event log; the live
//! runner composes the per-step primitives (`remove_orphan_outputs`,
//! `strip_unsupported_images`) before each turn because it cannot repair
//! mid-stream orphans the way replay can. Both paths share `protected_call_ids`,
//! `is_error_output`, and `total_tool_output_bytes` so the budget vocabulary
//! stays in one place.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::context::replay::{CLEARED_TOOL_OUTPUT_PLACEHOLDER, REDUCED_TOOL_OUTPUT_PREFIX};
use crate::context::replay_policy::ReplayBudget;
use crate::tool_registry::truncate::byte_prefix;

/// Marker text inserted as a `function_call_output` body for any
/// `function_call` whose recorded output is missing. Stable across builds so
/// inspectors and `/context` can classify the repaired item without an
/// out-of-band signal.
pub(crate) const ORPHAN_CALL_OUTPUT_PLACEHOLDER: &str =
    "[Tool result missing; original output was not recorded]";

/// Per-model knobs the history manager respects. Today this is just whether
/// the resolved model accepts image inputs; new flags should land here so
/// callers don't sprout ad-hoc model checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModelCapabilities {
    pub supports_images: bool,
}

impl ModelCapabilities {
    /// Capabilities that strip nothing — used as a default in tests and at
    /// surfaces that have not yet plumbed the resolved model name through.
    pub(crate) const fn permissive() -> Self {
        Self {
            supports_images: true,
        }
    }

    /// Map a model name to its known capabilities. The list is intentionally
    /// short: nav adds an entry only when a model actually rejects the
    /// modality on submit. Unknown families default to permissive so the
    /// provider error (rather than a silent strip) surfaces the mismatch.
    pub(crate) fn for_model(name: &str) -> Self {
        let normalized = name.trim().to_ascii_lowercase();
        // OpenAI's "mini" reasoning models historically reject image input.
        // Match on a family prefix instead of an exact slug so dated names
        // (e.g. `o3-mini-2025-01-31`) are covered without a maintenance list.
        let text_only = normalized.starts_with("o1-mini") || normalized.starts_with("o3-mini");
        Self {
            supports_images: !text_only,
        }
    }
}

/// Remove any `function_call_output` whose `call_id` does not match a
/// `function_call` present in `input`. The Responses API rejects an output
/// without its call, and replay can produce them when a continuation event
/// was lost. Both the live loop and replay call this before submitting.
pub(crate) fn remove_orphan_outputs(input: &mut Vec<Value>) {
    let call_ids: HashSet<String> = input
        .iter()
        .filter(|item| item_type(item) == Some("function_call"))
        .filter_map(|item| call_id(item).map(str::to_string))
        .collect();
    input.retain(|item| {
        item_type(item) != Some("function_call_output")
            || call_id(item).is_some_and(|id| call_ids.contains(id))
    });
}

/// Append a placeholder `function_call_output` for any `function_call` whose
/// recorded output is missing. Repair (rather than removal) preserves the
/// surrounding reasoning continuation: dropping the call would leave the
/// `id` it referenced dangling and break the next `store: false` turn.
/// Returns the number of synthesized outputs.
pub(crate) fn repair_orphan_calls(input: &mut Vec<Value>) -> usize {
    let recorded_outputs: HashSet<&str> = input
        .iter()
        .filter(|item| item_type(item) == Some("function_call_output"))
        .filter_map(call_id)
        .collect();
    // Borrow checker: filter on borrowed `call_id` references against
    // `recorded_outputs`, then `to_string` only the survivors so the common
    // zero-orphan path allocates nothing.
    let orphan_ids: Vec<String> = input
        .iter()
        .filter(|item| item_type(item) == Some("function_call"))
        .filter_map(call_id)
        .filter(|id| !recorded_outputs.contains(*id))
        .map(str::to_string)
        .collect();
    let count = orphan_ids.len();
    for id in orphan_ids {
        input.push(json!({
            "type": "function_call_output",
            "call_id": id,
            "output": ORPHAN_CALL_OUTPUT_PLACEHOLDER,
        }));
    }
    count
}

/// Strip `input_image` parts from user messages when the resolved model does
/// not accept image input. Empty content arrays are left in place so the
/// surrounding `input_text` still ships and the message stays well-formed.
/// Returns the number of parts removed.
pub(crate) fn strip_unsupported_images(
    input: &mut [Value],
    capabilities: &ModelCapabilities,
) -> usize {
    if capabilities.supports_images {
        return 0;
    }
    let mut stripped = 0;
    for item in input.iter_mut() {
        if item_type(item) != Some("message") {
            continue;
        }
        let Some(Value::Array(parts)) = item.get_mut("content") else {
            continue;
        };
        let before = parts.len();
        parts.retain(|part| part.get("type").and_then(Value::as_str) != Some("input_image"));
        stripped += before - parts.len();
    }
    stripped
}

/// Apply the deterministic replay output policy: reduce eligible tool outputs
/// to a compact header + preview, then clear oldest unprotected outputs once
/// total bytes still exceed the global budget. Live and replay share this so
/// the input shipped to the provider matches the shape `/context` reports.
///
/// Callers should ensure `input` is paired (e.g. via `remove_orphan_outputs`)
/// before invoking; this function does not re-pair, so it never produces new
/// orphans on its own.
pub(crate) fn apply_replay_budget(input: &mut [Value], budget: &ReplayBudget) {
    if input.is_empty() {
        return;
    }

    let tool_names = tool_names_by_call_id(input);
    let protected = protected_call_ids(input, budget.raw_tool_turns);

    for idx in function_call_output_indices(input) {
        let Some(call_id_owned) = call_id(&input[idx]).map(str::to_string) else {
            continue;
        };
        if protected.contains(&call_id_owned) {
            continue;
        }
        let Some(tool_name) = tool_names.get(&call_id_owned).map(String::as_str) else {
            continue;
        };
        if should_reduce_tool_output(tool_name, &input[idx]) {
            let output = output_text(&input[idx]).unwrap_or_default();
            let reduced = reduced_tool_output(tool_name, output, budget.max_raw_tool_output_bytes);
            set_output_text(&mut input[idx], reduced);
        }
    }

    clear_to_total_budget(input, &protected, budget.max_total_tool_output_bytes);
}

/// Single normalization pass run by replay before a Responses request is
/// submitted. The live runner composes the per-step primitives because it
/// cannot repair mid-stream orphans the way replay can.
pub(crate) fn normalize_for_request(
    input: &mut Vec<Value>,
    capabilities: &ModelCapabilities,
    budget: &ReplayBudget,
) {
    remove_orphan_outputs(input);
    repair_orphan_calls(input);
    apply_replay_budget(input, budget);
    strip_unsupported_images(input, capabilities);
}

/// `function_call` ids in the most recent `raw_tool_turns` user-message
/// boundaries. Both the live pruner (which drops whole pairs) and the replay
/// budget (which reduces/clears outputs) protect this set so the conversation
/// near the current turn keeps its raw tool I/O.
pub(crate) fn protected_call_ids(input: &[Value], raw_tool_turns: usize) -> HashSet<String> {
    let mut protected: HashSet<String> = HashSet::new();
    if raw_tool_turns == 0 {
        return protected;
    }

    let user_indices: Vec<usize> = input
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item_type(item) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("user")
        })
        .map(|(idx, _)| idx)
        .collect();
    if user_indices.is_empty() {
        return protected;
    }

    let take = raw_tool_turns.min(user_indices.len());
    let protect_from = user_indices[user_indices.len() - take];
    for item in &input[protect_from..] {
        if item_type(item) == Some("function_call")
            && let Some(id) = call_id(item)
        {
            protected.insert(id.to_string());
        }
    }
    protected
}

pub(crate) fn item_type(item: &Value) -> Option<&str> {
    item.get("type").and_then(Value::as_str)
}

pub(crate) fn call_id(item: &Value) -> Option<&str> {
    item.get("call_id").and_then(Value::as_str)
}

fn output_text(item: &Value) -> Option<&str> {
    item.get("output").and_then(Value::as_str)
}

fn tool_names_by_call_id(input: &[Value]) -> HashMap<String, String> {
    input
        .iter()
        .filter(|item| item_type(item) == Some("function_call"))
        .filter_map(|item| {
            let id = call_id(item)?;
            let name = item.get("name").and_then(Value::as_str)?;
            Some((id.to_string(), name.to_string()))
        })
        .collect()
}

fn function_call_output_indices(input: &[Value]) -> Vec<usize> {
    input
        .iter()
        .enumerate()
        .filter(|(_, item)| item_type(item) == Some("function_call_output"))
        .map(|(idx, _)| idx)
        .collect()
}

fn should_reduce_tool_output(tool_name: &str, item: &Value) -> bool {
    if is_error_output(item) || output_is_already_budgeted(item) {
        return false;
    }
    matches!(tool_name, "read_file" | "code_search" | "bash")
}

fn output_is_already_budgeted(item: &Value) -> bool {
    output_text(item).is_some_and(|text| {
        text.starts_with(CLEARED_TOOL_OUTPUT_PLACEHOLDER)
            || text.starts_with(REDUCED_TOOL_OUTPUT_PREFIX)
            || text.starts_with(ORPHAN_CALL_OUTPUT_PLACEHOLDER)
    })
}

pub(crate) fn is_error_output(item: &Value) -> bool {
    let text = output_text(item).unwrap_or_default();
    text.starts_with("tool error:") || (text.starts_with("tool ") && text.contains(" blocked:"))
}

fn reduced_tool_output(tool_name: &str, output: &str, max_bytes: usize) -> String {
    let header = if output.len() > max_bytes {
        format!(
            "{REDUCED_TOOL_OUTPUT_PREFIX}: {tool_name}; original {} bytes; showing first {max_bytes} bytes]\n",
            output.len(),
        )
    } else {
        format!(
            "{REDUCED_TOOL_OUTPUT_PREFIX}: {tool_name}; original {} bytes]\n",
            output.len()
        )
    };
    let preview_budget = max_bytes.saturating_sub(header.len());
    let preview = byte_prefix(output, preview_budget);
    let mut reduced = String::with_capacity(header.len() + preview.len());
    reduced.push_str(&header);
    reduced.push_str(preview);
    reduced
}

fn clear_to_total_budget(input: &mut [Value], protected: &HashSet<String>, max_total_bytes: usize) {
    let mut total = total_tool_output_bytes(input);
    if total <= max_total_bytes {
        return;
    }

    for idx in function_call_output_indices(input) {
        if total <= max_total_bytes {
            break;
        }
        let Some(id) = call_id(&input[idx]) else {
            continue;
        };
        if protected.contains(id) {
            continue;
        }
        let old_len = output_text(&input[idx]).map(str::len).unwrap_or(0);
        set_output_text(&mut input[idx], CLEARED_TOOL_OUTPUT_PLACEHOLDER.to_string());
        total = total
            .saturating_sub(old_len)
            .saturating_add(CLEARED_TOOL_OUTPUT_PLACEHOLDER.len());
    }
}

pub(crate) fn total_tool_output_bytes(input: &[Value]) -> usize {
    input
        .iter()
        .filter(|item| item_type(item) == Some("function_call_output"))
        .filter_map(output_text)
        .map(str::len)
        .sum()
}

fn set_output_text(item: &mut Value, output: String) {
    if let Value::Object(fields) = item {
        fields.insert("output".to_string(), Value::String(output));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_msg(text: &str) -> Value {
        json!({"type": "message", "role": "user", "content": text})
    }

    fn user_msg_with_image(text: &str, image_url: &str) -> Value {
        json!({
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": text},
                {"type": "input_image", "image_url": image_url},
            ],
        })
    }

    fn assistant_msg(text: &str) -> Value {
        json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        })
    }

    fn call(id: &str) -> Value {
        json!({"type": "function_call", "call_id": id, "name": "bash", "arguments": "{}"})
    }

    fn output(id: &str, body: &str) -> Value {
        json!({"type": "function_call_output", "call_id": id, "output": body})
    }

    #[test]
    fn capabilities_for_known_text_only_families() {
        assert!(!ModelCapabilities::for_model("o1-mini").supports_images);
        assert!(!ModelCapabilities::for_model("o3-mini-2025-01-31").supports_images);
        assert!(ModelCapabilities::for_model("gpt-5").supports_images);
        assert!(ModelCapabilities::for_model("gpt-4o").supports_images);
        // Unknown families default to permissive so the provider's own
        // rejection surfaces — silent stripping would hide the mismatch.
        assert!(ModelCapabilities::for_model("future-model").supports_images);
    }

    #[test]
    fn remove_orphan_outputs_drops_outputs_without_matching_call() {
        let mut input = vec![
            user_msg("hi"),
            call("c1"),
            output("c1", "kept"),
            output("c2", "dropped"),
        ];
        remove_orphan_outputs(&mut input);
        let call_ids: Vec<&str> = input
            .iter()
            .filter(|item| item_type(item) == Some("function_call_output"))
            .filter_map(call_id)
            .collect();
        assert_eq!(call_ids, vec!["c1"]);
    }

    #[test]
    fn repair_orphan_calls_appends_placeholder_output() {
        let mut input = vec![user_msg("hi"), call("c1"), call("c2"), output("c2", "ok")];
        let repaired = repair_orphan_calls(&mut input);
        assert_eq!(repaired, 1);
        let placeholder = input
            .iter()
            .find(|item| {
                item_type(item) == Some("function_call_output") && call_id(item) == Some("c1")
            })
            .expect("synthesized output for c1");
        assert_eq!(
            output_text(placeholder),
            Some(ORPHAN_CALL_OUTPUT_PLACEHOLDER)
        );
    }

    #[test]
    fn repair_orphan_calls_is_idempotent() {
        let mut input = vec![call("c1")];
        let first = repair_orphan_calls(&mut input);
        let second = repair_orphan_calls(&mut input);
        assert_eq!(first, 1);
        assert_eq!(second, 0, "second pass must find no orphans");
    }

    #[test]
    fn strip_unsupported_images_removes_input_image_parts() {
        let mut input = vec![
            user_msg_with_image("look", "data:image/png;base64,abc"),
            assistant_msg("ok"),
        ];
        let capabilities = ModelCapabilities {
            supports_images: false,
        };
        let stripped = strip_unsupported_images(&mut input, &capabilities);
        assert_eq!(stripped, 1);
        let parts = input[0].get("content").and_then(Value::as_array).unwrap();
        assert!(
            parts
                .iter()
                .all(|part| part.get("type").and_then(Value::as_str) != Some("input_image"))
        );
        assert!(
            parts
                .iter()
                .any(|part| part.get("type").and_then(Value::as_str) == Some("input_text"))
        );
    }

    #[test]
    fn strip_unsupported_images_noop_when_model_supports_images() {
        let mut input = vec![user_msg_with_image("look", "data:image/png;base64,abc")];
        let capabilities = ModelCapabilities::permissive();
        let stripped = strip_unsupported_images(&mut input, &capabilities);
        assert_eq!(stripped, 0);
        let parts = input[0].get("content").and_then(Value::as_array).unwrap();
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn normalize_for_request_pairs_and_strips_when_needed() {
        let mut input = vec![
            user_msg_with_image("see this", "data:image/png;base64,abc"),
            call("c1"),
            output("c1", "ok"),
            output("c-orphan", "should be dropped"),
            call("c2"),
        ];
        let capabilities = ModelCapabilities {
            supports_images: false,
        };
        normalize_for_request(&mut input, &capabilities, &ReplayBudget::default());

        let outputs: Vec<&str> = input
            .iter()
            .filter(|item| item_type(item) == Some("function_call_output"))
            .filter_map(call_id)
            .collect();
        assert!(outputs.contains(&"c1"), "real output kept: {outputs:?}");
        assert!(
            outputs.contains(&"c2"),
            "orphan call repaired: {outputs:?}"
        );
        assert!(
            !outputs.contains(&"c-orphan"),
            "stray output removed: {outputs:?}"
        );

        let parts = input[0].get("content").and_then(Value::as_array).unwrap();
        assert!(
            parts
                .iter()
                .all(|part| part.get("type").and_then(Value::as_str) != Some("input_image"))
        );
    }

    #[test]
    fn protected_call_ids_keeps_recent_turns() {
        let body = "x".repeat(10);
        let input = vec![
            user_msg("old"),
            call("c1"),
            output("c1", &body),
            user_msg("recent"),
            call("c2"),
            output("c2", &body),
        ];
        let protected = protected_call_ids(&input, 1);
        assert!(protected.contains("c2"));
        assert!(!protected.contains("c1"));
    }

    #[test]
    fn apply_replay_budget_reduces_and_clears_oldest_unprotected_outputs() {
        let large = "x".repeat(70 * 1024);
        let mut input = vec![
            user_msg("old"),
            json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{}"}),
            output("c1", &large),
            user_msg("mid"),
            json!({"type": "function_call", "call_id": "c2", "name": "bash", "arguments": "{}"}),
            output("c2", &large),
            user_msg("recent"),
            json!({"type": "function_call", "call_id": "c3", "name": "bash", "arguments": "{}"}),
            output("c3", "small recent"),
        ];
        apply_replay_budget(&mut input, &ReplayBudget::default());

        let recent = input
            .iter()
            .rev()
            .find(|item| item_type(item) == Some("function_call_output"))
            .unwrap();
        assert_eq!(output_text(recent), Some("small recent"));
        let oldest_output = input
            .iter()
            .find(|item| {
                item_type(item) == Some("function_call_output") && call_id(item) == Some("c1")
            })
            .unwrap();
        let text = output_text(oldest_output).unwrap();
        assert!(
            text.starts_with(CLEARED_TOOL_OUTPUT_PLACEHOLDER)
                || text.starts_with(REDUCED_TOOL_OUTPUT_PREFIX),
            "oldest must be cleared or reduced: {text:?}"
        );
    }
}
