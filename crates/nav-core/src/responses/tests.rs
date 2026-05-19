use super::collector::decode_completed_response;
use super::parser::function_calls;
use super::types::{MessagePart, ResponseEnvelope, ResponseItem};
use super::*;
use crate::agent::TurnUsage;
use crate::cli::Args;
use crate::project::{ContextFile, ContextScope, ProjectContext};
use crate::skills::Catalog;
use serde_json::json;
use std::path::PathBuf;

// ── detect_context_overflow ───────────────────────────────────

#[test]
fn detect_context_overflow_matches_top_level_error() {
    let event = json!({
        "type": "error",
        "code": "context_length_exceeded",
        "message": "Your input exceeds the model's context"
    });
    let msg = detect_context_overflow(&event).expect("should detect overflow");
    assert!(msg.contains("exceeds the model's context"));
}

#[test]
fn detect_context_overflow_matches_response_failed_shape() {
    let event = json!({
        "type": "response.failed",
        "response": {
            "error": {
                "code": "context_window_exceeded",
                "message": "Too long"
            }
        }
    });
    let msg = detect_context_overflow(&event).expect("should detect overflow");
    assert_eq!(msg, "Too long");
}

#[test]
fn detect_context_overflow_ignores_other_error_codes() {
    let event = json!({
        "type": "error",
        "code": "rate_limit_exceeded",
        "message": "Slow down"
    });
    assert!(detect_context_overflow(&event).is_none());
}

#[test]
fn detect_context_overflow_ignores_non_error_events() {
    let event = json!({"type": "response.completed", "response": {}});
    assert!(detect_context_overflow(&event).is_none());
}

#[test]
fn detect_http_overflow_matches_responses_api_error_body() {
    let body = r#"{"error":{"code":"context_length_exceeded","message":"Your input is 220k tokens; the model supports 200k."}}"#;
    let msg = detect_http_overflow(body).expect("should match");
    assert!(msg.contains("220k tokens"));
}

#[test]
fn detect_http_overflow_matches_window_alias() {
    let body = r#"{"error":{"code":"context_window_exceeded","message":"too long"}}"#;
    assert_eq!(detect_http_overflow(body).as_deref(), Some("too long"));
}

#[test]
fn detect_http_overflow_ignores_other_errors() {
    let body = r#"{"error":{"code":"invalid_request_error","message":"bad model"}}"#;
    assert!(detect_http_overflow(body).is_none());
}

#[test]
fn detect_http_overflow_ignores_non_json_body() {
    assert!(detect_http_overflow("not json at all").is_none());
    assert!(detect_http_overflow("").is_none());
}

// ── model_hint_from_body ──────────────────────────────────────

#[test]
fn model_hint_extracts_from_model_not_found_code() {
    let body =
        r#"{"error":{"code":"model_not_found","message":"The model `gpt-55` does not exist."}}"#;
    let hint = model_hint_from_body(body);
    assert!(
        hint.contains("Did you mean"),
        "expected suggestion in hint, got {hint:?}"
    );
    assert!(
        hint.contains("gpt-5.5"),
        "expected gpt-5.5 in hint, got {hint:?}"
    );
}

#[test]
fn model_hint_extracts_from_invalid_request_error() {
    // The user typed `gpt-4oo` (one extra `o`) — the suggestion list should
    // home in on `gpt-4o`.
    let body = r#"{"error":{"type":"invalid_request_error","message":"The model `gpt-4oo` does not exist or you do not have access to it.","code":null}}"#;
    let hint = model_hint_from_body(body);
    assert!(hint.contains("Did you mean"), "got hint {hint:?}");
    assert!(hint.contains("gpt-4o"));
}

#[test]
fn model_hint_skips_unrelated_errors() {
    let body = r#"{"error":{"code":"rate_limit_exceeded","message":"slow down"}}"#;
    assert_eq!(model_hint_from_body(body), "");
}

#[test]
fn model_hint_returns_empty_for_non_json() {
    assert_eq!(model_hint_from_body("not json"), "");
    assert_eq!(model_hint_from_body(""), "");
}

#[test]
fn responses_error_round_trips_to_anyhow() {
    let err = ResponsesError::ContextWindowExceeded {
        message: "boom".into(),
    };
    let anyhow_err: anyhow::Error = err.into();
    assert!(anyhow_err.to_string().contains("context window exceeded"));
    assert!(anyhow_err.to_string().contains("boom"));
}

// ── response_body ─────────────────────────────────────────────

#[test]
fn response_body_includes_model_and_store_false() {
    let args = Args::test_default();
    let cwd = std::path::Path::new("/tmp");
    let input = vec![json!({"type": "message", "role": "user", "content": "hi"})];
    let body = response_body(&args, cwd, &input, &Catalog::default(), None);
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["store"], false);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert!(body["input"].is_array());
    assert!(body["tools"].is_array());
}

#[test]
fn response_body_instructions_contain_cwd() {
    let args = Args::test_default();
    let cwd = std::path::Path::new("/my/project");
    let body = response_body(&args, cwd, &[], &Catalog::default(), None);
    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.contains("/my/project"));
    // No skills => no available-skills section.
    assert!(!instructions.contains("Available skills"));
}

#[test]
fn response_body_passes_input_through() {
    let args = Args::test_default();
    let cwd = std::path::Path::new("/tmp");
    let input = vec![
        json!({"type": "message", "role": "user", "content": "hello"}),
        json!({"type": "function_call_output", "call_id": "c1", "output": "ok"}),
    ];
    let body = response_body(&args, cwd, &input, &Catalog::default(), None);
    let body_input = body["input"].as_array().unwrap();
    assert_eq!(body_input.len(), 2);
    assert_eq!(body_input[1]["call_id"], "c1");
}

#[test]
fn response_body_lists_skills_when_present() {
    use crate::skills::{Skill, SkillScope};
    let args = Args::test_default();
    let cwd = std::path::Path::new("/tmp");
    let catalog = Catalog::new(vec![Skill {
        name: "demo".into(),
        description: "demo skill".into(),
        skill_md_path: "/abs/skills/demo/SKILL.md".into(),
        skill_dir: "/abs/skills/demo".into(),
        scope: SkillScope::Project,
    }]);
    let body = response_body(&args, cwd, &[], &catalog, None);
    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.contains("Available skills"));
    assert!(instructions.contains("demo"));
    assert!(instructions.contains("demo skill"));
    assert!(instructions.contains("/abs/skills/demo/SKILL.md"));
    assert!(instructions.contains("/abs/skills/demo"));
    assert!(instructions.contains("read the listed SKILL.md"));
}

#[test]
fn response_body_appends_context_files_in_user_then_project_order() {
    let args = Args::test_default();
    let cwd = std::path::Path::new("/tmp");
    let context = ProjectContext {
        context_files: vec![
            ContextFile {
                path: PathBuf::from("/home/me/.agents/AGENTS.md"),
                display_name: "AGENTS.md".into(),
                scope: ContextScope::User,
                bytes: "USER body".into(),
            },
            ContextFile {
                path: PathBuf::from("/tmp/AGENTS.md"),
                display_name: "AGENTS.md".into(),
                scope: ContextScope::Project,
                bytes: "PROJECT body".into(),
            },
        ],
        ..ProjectContext::default()
    };
    let body = response_body(&args, cwd, &[], &Catalog::default(), Some(&context));
    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.contains("Project context follows"));
    assert!(instructions.contains("USER body"));
    assert!(instructions.contains("PROJECT body"));
    // Project body must appear after the user body so it gets the strongest
    // recency anchor in the instructions.
    let user_idx = instructions.find("USER body").unwrap();
    let project_idx = instructions.find("PROJECT body").unwrap();
    assert!(user_idx < project_idx);
    assert!(instructions.contains("--- BEGIN AGENTS.md (user) ---"));
    assert!(instructions.contains("--- END AGENTS.md (project) ---"));
}

#[test]
fn response_body_omits_context_section_when_no_files() {
    let args = Args::test_default();
    let cwd = std::path::Path::new("/tmp");
    let context = ProjectContext::default();
    let body = response_body(&args, cwd, &[], &Catalog::default(), Some(&context));
    let instructions = body["instructions"].as_str().unwrap();
    assert!(!instructions.contains("Project context follows"));
}

// ── function_calls ────────────────────────────────────────────

#[test]
fn function_calls_extracts_single_call() {
    let items = vec![ResponseItem::FunctionCall {
        call_id: "c1".into(),
        name: "read_file".into(),
        arguments: r#"{"path":"foo.rs"}"#.into(),
    }];
    let calls = function_calls(&items).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "c1");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments["path"], "foo.rs");
}

#[test]
fn function_calls_extracts_multiple_calls() {
    let items = vec![
        ResponseItem::FunctionCall {
            call_id: "c1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a.rs"}"#.into(),
        },
        ResponseItem::FunctionCall {
            call_id: "c2".into(),
            name: "bash".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        },
    ];
    let calls = function_calls(&items).unwrap();
    assert_eq!(calls.len(), 2);
}

#[test]
fn function_calls_returns_empty_for_message_only() {
    let items = vec![ResponseItem::Message { content: None }];
    let calls = function_calls(&items).unwrap();
    assert!(calls.is_empty());
}

#[test]
fn function_calls_returns_empty_for_other_items() {
    let items = vec![ResponseItem::Other];
    let calls = function_calls(&items).unwrap();
    assert!(calls.is_empty());
}

#[test]
fn function_calls_rejects_invalid_arguments_json() {
    let items = vec![ResponseItem::FunctionCall {
        call_id: "c1".into(),
        name: "bash".into(),
        arguments: "not-json".into(),
    }];
    let err = function_calls(&items).unwrap_err();
    assert!(err.to_string().contains("failed to parse arguments"));
}

#[test]
fn function_calls_skips_non_call_items_but_extracts_calls() {
    let items = vec![
        ResponseItem::Message { content: None },
        ResponseItem::FunctionCall {
            call_id: "c1".into(),
            name: "bash".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        },
        ResponseItem::Other,
    ];
    let calls = function_calls(&items).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "bash");
}

// ── process_response ──────────────────────────────────────────

#[test]
fn process_response_extracts_function_calls_from_envelope() {
    let response = ResponseEnvelope {
        output: Some(vec![ResponseItem::FunctionCall {
            call_id: "c1".into(),
            name: "bash".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        }]),
        usage: None,
        raw_output: vec![],
    };
    let calls = process_response(&response).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "bash");
}

#[test]
// print_messages writes to stdout ("done"); cargo test captures it.
fn process_response_returns_empty_when_message_only() {
    let response = ResponseEnvelope {
        output: Some(vec![ResponseItem::Message {
            content: Some(vec![MessagePart::OutputText {
                text: "done".into(),
            }]),
        }]),
        usage: None,
        raw_output: vec![],
    };
    let calls = process_response(&response).unwrap();
    assert!(calls.is_empty());
}

#[test]
fn process_response_errors_on_no_output() {
    let response = ResponseEnvelope {
        output: None,
        usage: None,
        raw_output: vec![],
    };
    let err = process_response(&response).unwrap_err();
    assert!(err.to_string().contains("no output"));
}

// ── into_raw_output ───────────────────────────────────────────

#[test]
fn into_raw_output_returns_raw_items() {
    let response = ResponseEnvelope {
        output: Some(vec![]),
        usage: None,
        raw_output: vec![json!({"type": "function_call", "call_id": "c1"})],
    };
    let raw = into_raw_output(response);
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0]["call_id"], "c1");
}

// ── decode_completed_response ─────────────────────────────────

#[test]
fn decode_completed_response_parses_envelope() {
    let event = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_123",
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "hi"}]}
            ]
        }
    });
    let envelope = decode_completed_response(&event).unwrap();
    let output = envelope.output.as_ref().unwrap();
    assert_eq!(output.len(), 1);
    assert_eq!(envelope.raw_output.len(), 1);
    // Verify the message deserialized correctly, not just that it exists
    match &output[0] {
        ResponseItem::Message { content } => {
            let parts = content.as_ref().unwrap();
            match &parts[0] {
                MessagePart::OutputText { text } => assert_eq!(text, "hi"),
                other => panic!("expected OutputText, got {other:?}"),
            }
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

#[test]
fn decode_completed_response_errors_on_missing_response_field() {
    let event = json!({"type": "response.completed"});
    let err = decode_completed_response(&event).unwrap_err();
    assert!(err.to_string().contains("no response"));
}

// ── turn_usage_from ───────────────────────────────────────────

#[test]
fn turn_usage_from_returns_default_when_missing() {
    let response = ResponseEnvelope {
        output: None,
        usage: None,
        raw_output: vec![],
    };
    let usage = turn_usage_from(&response);
    assert_eq!(usage, TurnUsage::default());
}

#[test]
fn turn_usage_from_maps_all_fields() {
    let event = json!({
        "type": "response.completed",
        "response": {
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "input_tokens_details": {"cached_tokens": 20},
                "output_tokens_details": {"reasoning_tokens": 10}
            }
        }
    });
    let envelope = decode_completed_response(&event).unwrap();
    let usage = turn_usage_from(&envelope);
    assert_eq!(usage.tokens_input, 100);
    assert_eq!(usage.tokens_output, 50);
    assert_eq!(usage.tokens_input_cached, 20);
    assert_eq!(usage.tokens_reasoning, 10);
}

#[test]
fn turn_usage_from_missing_subfields_default_to_zero() {
    let event = json!({
        "type": "response.completed",
        "response": {
            "usage": {"input_tokens": 7}
        }
    });
    let envelope = decode_completed_response(&event).unwrap();
    let usage = turn_usage_from(&envelope);
    assert_eq!(usage.tokens_input, 7);
    assert_eq!(usage.tokens_output, 0);
    assert_eq!(usage.tokens_input_cached, 0);
    assert_eq!(usage.tokens_reasoning, 0);
}

// ── ResponseCollector ─────────────────────────────────────────

#[test]
fn collector_error_event_bails() {
    let mut collector = ResponseCollector::default();
    let event = json!({"type": "error", "message": "boom"});
    let err = collector.push_event(&event, "source").unwrap_err();
    assert!(err.to_string().contains("source returned error"));
}

#[test]
fn collector_completed_event_returns_true() {
    let mut collector = ResponseCollector::default();
    let event = json!({
        "type": "response.completed",
        "response": {"output": []}
    });
    let done = collector.push_event(&event, "src").unwrap();
    assert!(done);
    assert!(collector.completed.is_some());
}

#[test]
fn collector_output_item_done_appends_to_output() {
    let mut collector = ResponseCollector::default();
    let event = json!({
        "type": "response.output_item.done",
        "item": {"type": "message", "content": null}
    });
    let done = collector.push_event(&event, "src").unwrap();
    assert!(!done);
    assert_eq!(collector.output.len(), 1);
    assert_eq!(collector.raw_output.len(), 1);
}

#[test]
fn collector_ignores_unknown_events() {
    let mut collector = ResponseCollector::default();
    let event = json!({"type": "response.created"});
    let done = collector.push_event(&event, "src").unwrap();
    assert!(!done);
    assert!(collector.output.is_empty());
}

#[test]
fn collector_finish_errors_without_completed() {
    let collector = ResponseCollector::default();
    let err = collector.finish("test").unwrap_err();
    assert!(
        err.to_string()
            .contains("test ended without response.completed")
    );
}

#[test]
fn collector_finish_merges_streamed_output() {
    let mut collector = ResponseCollector::default();
    // In real streaming, output_item.done arrives before response.completed.
    // Push them in that order; the completed response has no output field,
    // so finish() should merge the streamed items.
    let output_event = json!({
        "type": "response.output_item.done",
        "item": {"type": "message", "content": [{"type": "output_text", "text": "hi"}]}
    });
    collector.push_event(&output_event, "src").unwrap();
    let completed_event = json!({
        "type": "response.completed",
        "response": {"id": "r1"}
    });
    collector.push_event(&completed_event, "src").unwrap();

    let result = collector.finish("src").unwrap();
    let output = result.output.as_ref().unwrap();
    assert_eq!(output.len(), 1);
}

#[test]
fn collector_finish_preserves_completed_output() {
    let mut collector = ResponseCollector::default();
    let completed_event = json!({
        "type": "response.completed",
        "response": {
            "output": [{"type": "message", "content": [{"type": "text", "text": "kept"}]}]
        }
    });
    collector.push_event(&completed_event, "src").unwrap();

    let result = collector.finish("src").unwrap();
    let output = result.output.as_ref().unwrap();
    assert_eq!(output.len(), 1);
    // The output from completed response is preserved, not replaced by empty streamed output
    assert!(matches!(&output[0], ResponseItem::Message { content } if content.is_some()));
}
