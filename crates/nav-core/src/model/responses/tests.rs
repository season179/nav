use super::collector::decode_completed_response;
use super::parser::function_calls;
use super::types::{MessagePart, ResponseEnvelope, ResponseItem};
use super::*;
use crate::agent_loop::TurnUsage;
use crate::cli::Args;
use crate::context::Catalog;
use crate::context::{ContextFile, ContextScope, ProjectContext};
use crate::tool_registry::{SPAWN_SUBAGENT_TOOL, ToolAccess};
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
    let hint = model_hint_from_body(body).expect("expected hint");
    assert!(hint.contains("Did you mean"), "got {hint:?}");
    assert!(hint.contains("gpt-5.5"), "got {hint:?}");
}

#[test]
fn model_hint_extracts_from_invalid_request_error() {
    let body = r#"{"error":{"type":"invalid_request_error","message":"The model `gpt-4oo` does not exist or you do not have access to it.","code":null}}"#;
    let hint = model_hint_from_body(body).expect("expected hint");
    assert!(hint.contains("Did you mean"), "got {hint:?}");
    assert!(hint.contains("gpt-4o"), "got {hint:?}");
}

#[test]
fn model_hint_skips_unrelated_errors() {
    let body = r#"{"error":{"code":"rate_limit_exceeded","message":"slow down"}}"#;
    assert_eq!(model_hint_from_body(body), None);
}

#[test]
fn model_hint_returns_none_for_non_json() {
    assert_eq!(model_hint_from_body("not json"), None);
    assert_eq!(model_hint_from_body(""), None);
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
    assert!(
        body["prompt_cache_key"]
            .as_str()
            .is_some_and(|k| k.starts_with("nav-"))
    );
}

#[test]
fn response_body_cache_key_partitions_by_stable_prefix() {
    let args = Args::test_default();
    let cwd_a = std::path::Path::new("/tmp/a");
    let cwd_b = std::path::Path::new("/tmp/b");
    let full_a = response_body(&args, cwd_a, &[], &Catalog::default(), None);
    let full_a_again = response_body(&args, cwd_a, &[], &Catalog::default(), None);
    let full_b = response_body(&args, cwd_b, &[], &Catalog::default(), None);
    let read_only_a = response_body_with_options(
        &args,
        cwd_a,
        &[],
        &Catalog::default(),
        None,
        ResponseBodyOptions::read_only(),
    );
    // Same inputs => same key (so routing groups identical-prefix requests).
    assert_eq!(full_a["prompt_cache_key"], full_a_again["prompt_cache_key"]);
    // Different instructions (cwd flows into instructions) => different key.
    assert_ne!(full_a["prompt_cache_key"], full_b["prompt_cache_key"]);
    // Different tool set => different key.
    assert_ne!(full_a["prompt_cache_key"], read_only_a["prompt_cache_key"]);
}

#[test]
fn response_body_instructions_contain_cwd() {
    let args = Args::test_default();
    let cwd = std::path::Path::new("/my/project");
    let body = response_body(&args, cwd, &[], &Catalog::default(), None);
    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.contains("/my/project"));
    assert!(instructions.contains("Keep responses concise"));
    assert!(instructions.contains("plain, layman's terms"));
    assert!(instructions.contains("Show file paths clearly"));
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
    use crate::context::{Skill, SkillScope};
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
    assert!(instructions.contains("- demo [project]: demo skill"));
    // Each skill entry advertises its discovered `skill_md_path` because
    // frontmatter `name` may differ from the directory basename; without
    // this the model would `read_file` a path that does not exist on disk.
    assert!(instructions.contains("(read: /abs/skills/demo/SKILL.md)"));
    assert!(instructions.contains("read its `SKILL.md`"));
}

#[test]
fn response_body_skill_section_lists_user_root_once_when_user_skills_present() {
    use crate::context::{Skill, SkillScope};
    let args = Args::test_default();
    let cwd = std::path::Path::new("/tmp");
    let catalog = Catalog::new(vec![
        Skill {
            name: "proj".into(),
            description: "project skill".into(),
            skill_md_path: "/work/.agents/skills/proj/SKILL.md".into(),
            skill_dir: "/work/.agents/skills/proj".into(),
            scope: SkillScope::Project,
        },
        Skill {
            name: "u1".into(),
            description: "user skill one".into(),
            skill_md_path: "/home/me/.agents/skills/u1/SKILL.md".into(),
            skill_dir: "/home/me/.agents/skills/u1".into(),
            scope: SkillScope::User,
        },
        Skill {
            name: "u2".into(),
            description: "user skill two".into(),
            skill_md_path: "/home/me/.agents/skills/u2/SKILL.md".into(),
            skill_dir: "/home/me/.agents/skills/u2".into(),
            scope: SkillScope::User,
        },
    ]);
    let body = response_body(&args, cwd, &[], &catalog, None);
    let instructions = body["instructions"].as_str().unwrap();
    // Each user-scoped skill now advertises its own SKILL.md path so
    // a discovered name that differs from the directory basename
    // still resolves; that means one mention per user skill, not one
    // collapsed root line.
    assert!(instructions.contains("(read: /home/me/.agents/skills/u1/SKILL.md)"));
    assert!(instructions.contains("(read: /home/me/.agents/skills/u2/SKILL.md)"));
}

#[test]
fn response_body_skill_section_advertises_per_skill_path() {
    // When frontmatter `name` differs from the directory basename,
    // discovery still keeps that skill, and the catalog must tell the
    // model where SKILL.md actually lives — not a derived
    // `.agents/skills/<frontmatter-name>/SKILL.md` that does not exist.
    use crate::context::{Skill, SkillScope};
    let args = Args::test_default();
    let cwd = std::path::Path::new("/tmp");
    let catalog = Catalog::new(vec![Skill {
        // Frontmatter name (what the model invokes).
        name: "renamed".into(),
        description: "demo".into(),
        // Directory uses the legacy folder name.
        skill_md_path: "/work/.agents/skills/legacy-dir/SKILL.md".into(),
        skill_dir: "/work/.agents/skills/legacy-dir".into(),
        scope: SkillScope::Project,
    }]);
    let body = response_body(&args, cwd, &[], &catalog, None);
    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.contains("(read: /work/.agents/skills/legacy-dir/SKILL.md)"));
    // The wrapper must not hard-code `.agents/skills/<name>/SKILL.md` —
    // that pattern fails the moment name != dir.
    assert!(!instructions.contains("`.agents/skills/<name>/SKILL.md`"));
}

#[test]
fn response_body_instructions_snapshot_with_skills() {
    use crate::context::{Skill, SkillScope};
    let args = Args::test_default();
    let cwd = std::path::Path::new("/work");
    let catalog = Catalog::new(vec![
        Skill {
            name: "review".into(),
            description: "Review a pull request".into(),
            skill_md_path: "/work/.agents/skills/review/SKILL.md".into(),
            skill_dir: "/work/.agents/skills/review".into(),
            scope: SkillScope::Project,
        },
        Skill {
            name: "verify".into(),
            description: "Verify changes by running the app".into(),
            skill_md_path: "/home/me/.agents/skills/verify/SKILL.md".into(),
            skill_dir: "/home/me/.agents/skills/verify".into(),
            scope: SkillScope::User,
        },
    ]);
    let body = response_body(&args, cwd, &[], &catalog, None);
    let instructions = body["instructions"].as_str().unwrap();
    insta::assert_snapshot!(instructions, @r"
    You are a small coding agent running in /work.

    Guidelines:
    - Use tools to inspect, edit, search, and verify code.
    - Prefer small, explicit steps.
    - Keep responses concise.
    - Explain technical details in plain, layman's terms.
    - Show file paths clearly when working with files.
    - Paths must be relative.

    Available skills (load each on demand):
    - review [project]: Review a pull request (read: /work/.agents/skills/review/SKILL.md)
    - verify [user]: Verify changes by running the app (read: /home/me/.agents/skills/verify/SKILL.md)
    When a user request matches a skill, read its `SKILL.md` (the `read:` path on the line above) first to load full instructions, then act. Resolve any relative resources mentioned in a SKILL.md against that skill's directory.
    ");
}

#[test]
fn response_body_shape_snapshot_marks_stable_blocks_cacheable() {
    let args = Args::test_default();
    let cwd = std::path::Path::new("/work");
    let input = vec![json!({"type": "message", "role": "user", "content": "hi"})];
    let body = response_body(&args, cwd, &input, &Catalog::default(), None);
    // Sort keys so the snapshot pins the cache-marker placement next to the
    // stable prefix (instructions, tools, model).
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    insta::assert_snapshot!(keys.join("\n"), @r"
    include
    input
    instructions
    model
    prompt_cache_key
    store
    tools
    ");
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

#[test]
fn response_body_options_gate_subagent_calls_with_subagent_toggle() {
    let without_subagents = ResponseBodyOptions {
        tool_access: ToolAccess::Full,
        include_subagents: false,
    };
    assert!(without_subagents.allows_tool("apply_patch"));
    assert!(!without_subagents.allows_tool(SPAWN_SUBAGENT_TOOL));

    assert!(ResponseBodyOptions::default().allows_tool(SPAWN_SUBAGENT_TOOL));
    assert!(ResponseBodyOptions::read_only().allows_tool("read_file"));
    assert!(!ResponseBodyOptions::read_only().allows_tool("apply_patch"));
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

// ── sanitize_continuation_items ───────────────────────────────

#[test]
fn sanitize_continuation_items_strips_hidden_plaintext_reasoning() {
    let raw = vec![
        json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{"type": "summary_text", "text": "thinking out loud"}],
            "content": [{"type": "reasoning_text", "text": "raw chain of thought"}],
            "encrypted_content": "enc-blob",
        }),
        json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "read_file",
            "arguments": "{\"path\":\"x\"}",
        }),
    ];
    let sanitized = sanitize_continuation_items(&raw);

    assert_eq!(sanitized.len(), 2);
    let reasoning = &sanitized[0];
    assert_eq!(reasoning["type"], "reasoning");
    assert_eq!(reasoning["id"], "rs_1");
    assert_eq!(reasoning["encrypted_content"], "enc-blob");
    assert!(reasoning.get("summary").is_none());
    assert!(reasoning.get("content").is_none());

    // function_call items pass through verbatim so the wire shape is intact.
    let call = &sanitized[1];
    assert_eq!(call["type"], "function_call");
    assert_eq!(call["call_id"], "call_1");
    assert_eq!(call["name"], "read_file");
}

#[test]
fn sanitize_continuation_items_drops_message_items() {
    // Assistant messages are persisted via `AssistantMessageDone`; keeping
    // them here too would replay the same message twice.
    let raw = vec![
        json!({
            "type": "message",
            "content": [{"type": "output_text", "text": "hi"}],
        }),
        json!({
            "type": "function_call",
            "call_id": "call_1",
            "name": "noop",
            "arguments": "{}",
        }),
    ];
    let sanitized = sanitize_continuation_items(&raw);

    assert_eq!(sanitized.len(), 1);
    assert_eq!(sanitized[0]["type"], "function_call");
}

#[test]
fn sanitize_continuation_items_drops_reasoning_without_encrypted_content() {
    // A reasoning item with no encrypted continuation handle has nothing
    // left to replay once the plaintext is stripped — drop it entirely so
    // the persisted event doesn't carry a useless structural shell.
    let raw = vec![json!({
        "type": "reasoning",
        "id": "rs_1",
        "summary": [{"type": "summary_text", "text": "thinking"}],
    })];
    let sanitized = sanitize_continuation_items(&raw);
    assert!(sanitized.is_empty());
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
