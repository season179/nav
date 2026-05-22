//! Chat Completions request body construction.
//!
//! Filled in by C1. The signature mirrors
//! [`crate::model::responses::request::response_body_with_options`] so the
//! agent loop can hand the same `(args, cwd, input, skills, context)` tuple to
//! either backend.

use crate::cli::Args;
use crate::context::{Catalog, ProjectContext};
use crate::model::auth::ResolvedProvider;
use serde_json::{Value, json};
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

/// Build the JSON body for `POST {base_url}/chat/completions`.
///
/// Stub: filled in by C1.
#[allow(dead_code)]
pub(crate) fn build_request_body(
    _args: &Args,
    _resolved: &ResolvedProvider,
    _cwd: &Path,
    _input: &[Value],
    _skills: &Catalog,
    _context: Option<&ProjectContext>,
) -> Value {
    unimplemented!("Chat Completions request body lands in C1")
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
