//! Chat Completions request body construction.
//!
//! Filled in by C1. The signature mirrors
//! [`crate::model::responses::request::response_body_with_options`] so the
//! agent loop can hand the same `(args, cwd, input, skills, context)` tuple to
//! either backend.

use crate::cli::Args;
use crate::context::build_instructions;
use crate::context::history::strip_synthetic_markers;
use crate::context::{Catalog, ProjectContext, ReasoningEffort};
use crate::model::auth::ResolvedProvider;
use crate::tool_registry::{ToolAccess, tool_definitions};
use serde_json::{Map, Value, json};
use std::path::Path;

/// Wrap Responses-style tool definitions into Chat Completions wire shape.
///
/// Responses API uses a flat layout:
/// ```json
/// { "type": "function", "name": "...", "description": "...", "parameters": {...} }
/// ```
///
/// Chat Completions nests `name`, `description`, and `parameters` under a
/// `function` key:
/// ```json
/// { "type": "function", "function": { "name": "...", "description": "...", "parameters": {...} } }
/// ```
///
/// Order is preserved. Each input value must be an object with at least
/// `name`, `description`, and `parameters` fields. The output `type` is
/// always `"function"`; any input `type` field is ignored.
#[allow(dead_code)]
pub(crate) fn wrap_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let name = tool.get("name").expect("tool must have a `name` field");
            let description = tool
                .get("description")
                .expect("tool must have a `description` field");
            let parameters = tool
                .get("parameters")
                .expect("tool must have a `parameters` field");

            json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters
                }
            })
        })
        .collect()
}

/// Resolve the effective reasoning effort: CLI flag > settings > model entry.
///
/// The CLI flag and settings override (`Args.reasoning_effort`) takes
/// precedence over the provider catalog's per-model default
/// (`ResolvedProvider.reasoning_effort`). When both are `None`, the field
/// is omitted from the request body entirely.
#[allow(dead_code)]
pub(crate) fn effective_reasoning_effort(
    args: &Args,
    resolved: &ResolvedProvider,
) -> Option<ReasoningEffort> {
    args.reasoning_effort.or(resolved.reasoning_effort)
}

/// Build the JSON body for `POST {base_url}/chat/completions`.
///
/// Mirrors [`crate::model::responses::request::response_body_with_options`]
/// but emits the Chat Completions wire shape: `messages` array with a leading
/// system message, `tools` wrapped under `function`, `stream: true`, and
/// optional `reasoning_effort` / `max_tokens`. No Responses-only knobs
/// (`store`, `include`, `prompt_cache_key`) are included.
///
/// `args` is accepted for signature parity with the Responses builder so the
/// agent loop can pass the same tuple to either backend, but the model name is
/// taken from `resolved.model_id` (which may differ from `args.model` when the
/// provider catalog overrides it).
#[allow(dead_code)] // Wired into the agent loop by G9.
pub(crate) fn build_request_body(
    args: &Args,
    resolved: &ResolvedProvider,
    cwd: &Path,
    input: &[Value],
    skills: &Catalog,
    context: Option<&ProjectContext>,
) -> Value {
    let instructions = build_instructions(cwd, skills, context);

    // System message is always first.
    let mut messages = vec![json!({
        "role": "system",
        "content": instructions,
    })];

    // Strip synthetic markers from the wire copy so providers never see them.
    let mut wire_input: Vec<Value> = input.to_vec();
    strip_synthetic_markers(&mut wire_input);

    // F3: convert persisted Responses-shape items into Chat Completions messages.
    let history = super::history::responses_items_to_chat_messages(&wire_input);
    messages.extend(history);

    // C3: wrap tool definitions into Chat Completions wire shape.
    let tools_defs = tool_definitions(ToolAccess::Full, true);
    let tools = wrap_tools(&tools_defs);

    let mut body = Map::new();
    body.insert("model".into(), Value::String(resolved.model_id.clone()));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("tools".into(), Value::Array(tools));
    body.insert("stream".into(), Value::Bool(true));

    if let Some(effort) = effective_reasoning_effort(args, resolved) {
        body.insert("reasoning_effort".into(), Value::String(effort.to_string()));
    }
    if let Some(max_tokens) = resolved.max_output_tokens {
        body.insert("max_tokens".into(), json!(max_tokens));
    }

    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Args;
    use crate::context::ReasoningEffort;
    use crate::model::auth::ResolvedProvider;
    use std::collections::BTreeMap;

    fn resolved(model_id: &str) -> ResolvedProvider {
        ResolvedProvider {
            base_url: "https://api.example.com/v1".into(),
            bearer: Some("sk-test".into()),
            headers: BTreeMap::new(),
            model_id: model_id.into(),
            reasoning_effort: None,
            max_output_tokens: None,
            display_name: format!("test/{model_id}"),
        }
    }

    fn resolved_with_effort(model_id: &str, effort: ReasoningEffort) -> ResolvedProvider {
        ResolvedProvider {
            reasoning_effort: Some(effort),
            ..resolved(model_id)
        }
    }

    fn resolved_with_max_tokens(model_id: &str, max_tokens: u32) -> ResolvedProvider {
        ResolvedProvider {
            max_output_tokens: Some(max_tokens),
            ..resolved(model_id)
        }
    }

    fn test_args(effort: Option<ReasoningEffort>) -> Args {
        let mut args = Args::test_default();
        args.reasoning_effort = effort;
        args
    }

    // ── wrap_tools tests ───────────────────────────────────────

    #[test]
    fn wrap_tools_moves_fields_under_function_key() {
        let input = vec![json!({
            "type": "function",
            "name": "read_file",
            "description": "Read a file.",
            "parameters": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }
        })];

        let wrapped = wrap_tools(&input);
        assert_eq!(wrapped.len(), 1);

        let tool = &wrapped[0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["function"]["name"], "read_file");
        assert_eq!(tool["function"]["description"], "Read a file.");
        assert_eq!(tool["function"]["parameters"]["type"], "object");
        // Top-level name/description/parameters must be absent.
        assert!(tool.get("name").is_none());
        assert!(tool.get("description").is_none());
        assert!(tool.get("parameters").is_none());
    }

    #[test]
    fn wrap_tools_preserves_order() {
        let input = vec![
            json!({
                "type": "function",
                "name": "bash",
                "description": "Run a command.",
                "parameters": { "type": "object" }
            }),
            json!({
                "type": "function",
                "name": "edit_file",
                "description": "Edit a file.",
                "parameters": { "type": "object" }
            }),
            json!({
                "type": "function",
                "name": "read_file",
                "description": "Read a file.",
                "parameters": { "type": "object" }
            }),
        ];

        let wrapped = wrap_tools(&input);
        let names: Vec<&str> = wrapped
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["bash", "edit_file", "read_file"]);
    }

    #[test]
    fn wrap_tools_drops_extra_top_level_fields() {
        // Extra top-level fields like `strict` are not carried into the
        // nested `function` object — we extract only name/description/
        // parameters explicitly.  `strict` support is out of scope per
        // issue #110.
        let input = vec![json!({
            "type": "function",
            "name": "search",
            "description": "Search.",
            "parameters": { "type": "object" },
            "strict": true
        })];

        let wrapped = wrap_tools(&input);
        assert_eq!(wrapped[0]["function"]["name"], "search");
        assert_eq!(wrapped[0]["function"]["description"], "Search.");
        assert!(wrapped[0]["function"].get("strict").is_none());
        // Input type is ignored — output is always "function".
        assert_eq!(wrapped[0]["type"], "function");
    }

    #[test]
    fn wrap_tools_ignores_input_type_field() {
        // Even if the input has a non-function type, output is always
        // "function" per the Chat Completions spec.
        let input = vec![json!({
            "type": "not_function",
            "name": "tool",
            "description": "A tool.",
            "parameters": { "type": "object" }
        })];

        let wrapped = wrap_tools(&input);
        assert_eq!(wrapped[0]["type"], "function");
    }

    #[test]
    fn wrap_tools_handles_empty_list() {
        let wrapped = wrap_tools(&[]);
        assert!(wrapped.is_empty());
    }

    // ── build_request_body snapshot tests ──────────────────────

    fn snap_body(body: &Value) -> String {
        // Remove tools — they're large and fully covered by wrap_tools tests.
        let mut redacted = body.clone();
        if let Some(obj) = redacted.as_object_mut() {
            obj.remove("tools");
        }
        serde_json::to_string_pretty(&redacted).unwrap()
    }

    #[test]
    fn simple_user_turn() {
        let args = Args::test_default();
        let resolved = resolved("glm-5.1");
        let cwd = std::path::Path::new("/tmp/project");
        let input = vec![json!({
            "type": "message",
            "role": "user",
            "content": "hello",
        })];

        let body = build_request_body(
            &args,
            &resolved,
            cwd,
            &input,
            &Default::default(),
            None,
        );

        insta::assert_snapshot!("simple_user_turn", snap_body(&body));

        // Verify structural invariants the snapshot won't catch.
        assert_eq!(body["stream"], true);
        assert_eq!(body["model"], "glm-5.1");
        assert!(body.get("store").is_none(), "no Responses-only store knob");
        assert!(body.get("include").is_none(), "no Responses-only include knob");
        assert!(body.get("prompt_cache_key").is_none(), "no prompt_cache_key");
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("max_tokens").is_none());

        // System message is first.
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert!(!messages[0]["content"].as_str().unwrap().is_empty());
        // Second message is the converted user turn.
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "hello");
    }

    #[test]
    fn turn_with_tool_call_in_history() {
        let args = Args::test_default();
        let resolved = resolved("glm-5.1");
        let cwd = std::path::Path::new("/tmp/project");
        let input = vec![
            json!({"type": "message", "role": "user", "content": "list files"}),
            json!({
                "type": "function_call",
                "call_id": "c1",
                "name": "bash",
                "arguments": "{\"command\":\"ls\"}",
            }),
            json!({"type": "function_call_output", "call_id": "c1", "output": "a.txt\nb.txt"}),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Two files."}],
            }),
        ];

        let body = build_request_body(
            &args,
            &resolved,
            cwd,
            &input,
            &Default::default(),
            None,
        );

        insta::assert_snapshot!("turn_with_tool_call", snap_body(&body));

        let messages = body["messages"].as_array().unwrap();
        // system, user, assistant+tool_calls, tool, assistant
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert!(messages[2]["tool_calls"].is_array());
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "c1");
        assert_eq!(messages[4]["role"], "assistant");
    }

    #[test]
    fn turn_with_reasoning_effort() {
        let args = Args::test_default();
        let resolved = resolved_with_effort("glm-5.1", ReasoningEffort::High);
        let cwd = std::path::Path::new("/tmp/project");
        let input = vec![json!({
            "type": "message",
            "role": "user",
            "content": "think hard",
        })];

        let body = build_request_body(
            &args,
            &resolved,
            cwd,
            &input,
            &Default::default(),
            None,
        );

        insta::assert_snapshot!("with_reasoning_effort", snap_body(&body));
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn turn_with_max_tokens() {
        let args = Args::test_default();
        let resolved = resolved_with_max_tokens("glm-5.1", 4096);
        let cwd = std::path::Path::new("/tmp/project");
        let input = vec![json!({
            "type": "message",
            "role": "user",
            "content": "short answer",
        })];

        let body = build_request_body(
            &args,
            &resolved,
            cwd,
            &input,
            &Default::default(),
            None,
        );

        insta::assert_snapshot!("with_max_tokens", snap_body(&body));
        assert_eq!(body["max_tokens"], 4096);
    }

    // ── effective_reasoning_effort tests (G5) ────────────────

    #[test]
    fn cli_flag_overrides_model_entry() {
        let resolved = resolved_with_effort("test", ReasoningEffort::Low);
        let args = test_args(Some(ReasoningEffort::High));
        assert_eq!(
            effective_reasoning_effort(&args, &resolved),
            Some(ReasoningEffort::High)
        );
    }

    #[test]
    fn model_entry_used_when_flag_absent() {
        let resolved = resolved_with_effort("test", ReasoningEffort::Medium);
        let args = test_args(None);
        assert_eq!(
            effective_reasoning_effort(&args, &resolved),
            Some(ReasoningEffort::Medium)
        );
    }

    #[test]
    fn none_when_both_absent() {
        let resolved = resolved("test");
        let args = test_args(None);
        assert_eq!(effective_reasoning_effort(&args, &resolved), None);
    }

    #[test]
    fn cli_flag_used_when_model_entry_absent() {
        let resolved = resolved("test");
        let args = test_args(Some(ReasoningEffort::Low));
        assert_eq!(
            effective_reasoning_effort(&args, &resolved),
            Some(ReasoningEffort::Low)
        );
    }

    #[test]
    fn reasoning_effort_cli_flag_in_build_request_body() {
        // The CLI flag should override the model entry in the full body too.
        let resolved = resolved_with_effort("glm-5.1", ReasoningEffort::Low);
        let mut args = Args::test_default();
        args.reasoning_effort = Some(ReasoningEffort::High);
        let cwd = std::path::Path::new("/tmp/project");
        let input = vec![json!({
            "type": "message",
            "role": "user",
            "content": "override test",
        })];

        let body = build_request_body(
            &args,
            &resolved,
            cwd,
            &input,
            &Default::default(),
            None,
        );

        assert_eq!(body["reasoning_effort"], "high");
    }
}
