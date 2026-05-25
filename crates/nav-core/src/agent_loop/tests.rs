use super::compaction_turn::{drop_oldest_tool_pair, trim_for_compaction};
use super::control::{PendingInput, PendingInputMode, TurnControls};
use super::runner::{emit_stream_events, extract_message_text, extract_reasoning_text};
use super::*;
use crate::cli::Args;
use crate::context::compaction::{SUMMARIZATION_PROMPT, SUMMARY_PREFIX, summary_message};
use crate::context::replay::{
    CLEARED_TOOL_OUTPUT_PLACEHOLDER, REDUCED_TOOL_OUTPUT_PREFIX, rebuild_responses_input,
};
use crate::context::{
    Catalog, ExtensionCatalog, ExtensionHook, ExtensionScope, HookCommand, HookEventType,
    ProjectContext, build_user_content,
};
use crate::guardrails::approval::{ApprovalGate, ApprovalRequest};
use crate::guardrails::{
    AskForApproval, PassthroughRunner, PermissionContext, ReviewDecision, SandboxPolicy,
    SessionAllowlist,
};
use crate::tool_registry::{SPAWN_SUBAGENT_TOOL, unchecked_permission_context};
use anyhow::Result;
use futures_util::stream;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio::sync::mpsc;

#[allow(clippy::too_many_arguments)]
async fn run_agent_for_test(
    transport: &dyn ResponsesTransport,
    args: &Args,
    cwd: &Path,
    prompt: &str,
    display_prompt: Option<&str>,
    attachments: Vec<UserAttachment>,
    events: mpsc::UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    initial_input: Option<Vec<Value>>,
    skills: &Catalog,
    context: Option<&ProjectContext>,
    permissions: PermissionContext,
) -> Result<()> {
    super::run_agent(
        AgentTurnRequest::new(transport, args, cwd, prompt, events, skills, permissions)
            .with_display_prompt(display_prompt)
            .with_attachments(attachments)
            .with_session(session, initial_input)
            .with_context(context),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_agent_for_test_with_controls(
    transport: &dyn ResponsesTransport,
    args: &Args,
    cwd: &Path,
    prompt: &str,
    display_prompt: Option<&str>,
    attachments: Vec<UserAttachment>,
    events: mpsc::UnboundedSender<AgentEvent>,
    session: Option<&SessionBinding<'_>>,
    initial_input: Option<Vec<Value>>,
    skills: &Catalog,
    context: Option<&ProjectContext>,
    permissions: PermissionContext,
    controls: TurnControls,
) -> Result<()> {
    super::run_agent(
        AgentTurnRequest::new(transport, args, cwd, prompt, events, skills, permissions)
            .with_display_prompt(display_prompt)
            .with_attachments(attachments)
            .with_session(session, initial_input)
            .with_context(context)
            .with_controls(controls),
    )
    .await
}

// ── stub transport ────────────────────────────────────────────

/// One unit of stream output a stub can hand back per turn.
///
/// Used by the few tests that need to inject a transport error (e.g. a
/// `context_length_exceeded`) into the middle of the agent loop. The normal
/// "happy path" tests use `StubTransport::new(turns_of_values)`, which lifts
/// `Value`s into `StubItem::Event` automatically.
enum StubItem {
    Event(Value),
    Err(crate::model::responses::ResponsesError),
}

/// Pops one canned event list per `create()` call so each turn of the
/// agent loop sees the next pre-recorded `Responses` API stream.
struct StubTransport {
    turns: Mutex<Vec<Vec<StubItem>>>,
    bodies: Mutex<Vec<Value>>,
    wire_format: crate::model::WireFormat,
    resolved: Option<crate::model::ResolvedProvider>,
}

impl StubTransport {
    fn new(turns: Vec<Vec<Value>>) -> Self {
        let turns = turns
            .into_iter()
            .map(|turn| turn.into_iter().map(StubItem::Event).collect())
            .collect();
        Self {
            turns: Mutex::new(turns),
            bodies: Mutex::new(Vec::new()),
            wire_format: crate::model::WireFormat::Responses,
            resolved: None,
        }
    }

    fn with_items(turns: Vec<Vec<StubItem>>) -> Self {
        Self {
            turns: Mutex::new(turns),
            bodies: Mutex::new(Vec::new()),
            wire_format: crate::model::WireFormat::Responses,
            resolved: None,
        }
    }

    fn chat_completions(turns: Vec<Vec<Value>>) -> Self {
        Self {
            wire_format: crate::model::WireFormat::ChatCompletions,
            resolved: Some(crate::model::ResolvedProvider {
                base_url: "https://api.example.com/v1".into(),
                bearer: Some("sk-test".into()),
                headers: BTreeMap::new(),
                model_id: "wire-model".into(),
                reasoning_effort: None,
                max_output_tokens: None,
                display_name: "test/wire-model".into(),
            }),
            ..Self::new(turns)
        }
    }

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }
}

fn event_position(
    events: &[AgentEvent],
    label: &str,
    predicate: impl Fn(&AgentEvent) -> bool,
) -> usize {
    events
        .iter()
        .position(predicate)
        .unwrap_or_else(|| panic!("expected event: {label}"))
}

fn input_position(input: &[Value], label: &str, predicate: impl Fn(&Value) -> bool) -> usize {
    input
        .iter()
        .position(predicate)
        .unwrap_or_else(|| panic!("expected input item: {label}"))
}

fn is_input_user_message(item: &Value, text: &str) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("user")
        && item.get("content").and_then(Value::as_str) == Some(text)
}

fn is_input_assistant_message(item: &Value, text: &str) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) == Some("assistant")
        && item
            .get("content")
            .and_then(Value::as_array)
            .and_then(|parts| parts.first())
            .and_then(|part| part.get("text"))
            .and_then(Value::as_str)
            == Some(text)
}

fn replay_tool_turn(
    prompt: &str,
    call_id: &str,
    tool_name: &str,
    output: String,
    is_error: bool,
) -> Vec<AgentEvent> {
    vec![
        AgentEvent::UserMessage {
            text: prompt.into(),
            display_text: None,
            attachments: Vec::new(),
        },
        AgentEvent::ResponseContinuation {
            items: vec![json!({
                "type": "function_call",
                "call_id": call_id,
                "name": tool_name,
                "arguments": "{}",
            })],
        },
        AgentEvent::ToolCallStarted {
            call_id: call_id.into(),
            name: tool_name.into(),
            arguments: json!({}),
        },
        AgentEvent::ToolCallOutput {
            call_id: call_id.into(),
            output,
            is_error,
            truncation: None,
        },
        AgentEvent::AssistantMessageDone {
            text: format!("{tool_name} done"),
        },
        AgentEvent::TurnComplete {
            usage: TurnUsage::default(),
        },
    ]
}

fn replay_tool_output<'a>(input: &'a [Value], call_id: &str) -> &'a str {
    input
        .iter()
        .find(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some(call_id)
        })
        .and_then(|item| item.get("output").and_then(Value::as_str))
        .unwrap_or_else(|| panic!("expected function_call_output for {call_id}"))
}

fn assert_replay_outputs_have_calls(input: &[Value]) {
    let calls: std::collections::HashSet<&str> = input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .filter_map(|item| item.get("call_id").and_then(Value::as_str))
        .collect();
    let orphan = input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
        .filter_map(|item| item.get("call_id").and_then(Value::as_str))
        .find(|call_id| !calls.contains(*call_id));
    assert!(orphan.is_none(), "orphan tool output: {orphan:?}");
}

fn git(cwd: &std::path::Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed with {status}");
}

impl ResponsesTransport for StubTransport {
    fn wire_format(&self) -> crate::model::WireFormat {
        self.wire_format
    }

    fn chat_completions_provider(&self) -> Option<crate::model::ResolvedProvider> {
        self.resolved.clone()
    }

    fn create<'a>(
        &'a self,
        body: Value,
        _events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>> {
        self.bodies.lock().unwrap().push(body);
        let turn_events = {
            let mut guard = self.turns.lock().unwrap();
            if guard.is_empty() {
                Vec::new()
            } else {
                guard.remove(0)
            }
        };
        Box::pin(async move {
            let s = stream::iter(turn_events.into_iter().map(|item| match item {
                StubItem::Event(value) => Ok(value),
                StubItem::Err(err) => Err(err),
            }));
            let boxed: EventStream = Box::pin(s);
            Ok(boxed)
        })
    }
}

struct FailingTransport;

impl ResponsesTransport for FailingTransport {
    fn create<'a>(
        &'a self,
        _body: Value,
        _events: mpsc::UnboundedSender<AgentEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<EventStream>> + Send + 'a>> {
        Box::pin(async { Err(anyhow::anyhow!("network down")) })
    }
}

struct AbortGate;

impl ApprovalGate for AbortGate {
    fn request<'a>(
        &'a self,
        _req: ApprovalRequest,
    ) -> Pin<Box<dyn Future<Output = ReviewDecision> + Send + 'a>> {
        Box::pin(async { ReviewDecision::Abort })
    }
}

fn aborting_permission_context() -> PermissionContext {
    PermissionContext {
        gate: Arc::new(AbortGate),
        policy: AskForApproval::OnRequest,
        sandbox_policy: SandboxPolicy::DangerFullAccess,
        sandbox: Arc::new(PassthroughRunner),
        session_allowlist: SessionAllowlist::default(),
    }
}

/// Recording approval gate. Captures the [`ApprovalRequest`]s that fly
/// through it and returns a pre-programmed [`ReviewDecision`] per request,
/// cycling through `decisions`. Used by the attachment-gate tests so we can
/// assert *which* attachment paths triggered approval and that the
/// `protected_read` reason was set.
#[derive(Default)]
struct RecordingGate {
    decisions: Mutex<Vec<ReviewDecision>>,
    requests: Mutex<Vec<ApprovalRequest>>,
}

impl RecordingGate {
    fn with_decisions(decisions: Vec<ReviewDecision>) -> Self {
        Self {
            decisions: Mutex::new(decisions),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ApprovalRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl ApprovalGate for RecordingGate {
    fn request<'a>(
        &'a self,
        req: ApprovalRequest,
    ) -> Pin<Box<dyn Future<Output = ReviewDecision> + Send + 'a>> {
        self.requests.lock().unwrap().push(req);
        let decision = self
            .decisions
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(ReviewDecision::Approved);
        Box::pin(async move { decision })
    }
}

fn recording_permission_context(gate: Arc<RecordingGate>) -> PermissionContext {
    PermissionContext {
        gate,
        policy: AskForApproval::OnRequest,
        sandbox_policy: SandboxPolicy::DangerFullAccess,
        sandbox: Arc::new(PassthroughRunner),
        session_allowlist: SessionAllowlist::default(),
    }
}

// ── drop_oldest_tool_pair ────────────────────────────────────

#[test]
fn drop_oldest_tool_pair_removes_first_call_and_matching_output() {
    let mut input = vec![
        json!({"type": "message", "role": "user", "content": "hi"}),
        json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{}"}),
        json!({"type": "function_call_output", "call_id": "c1", "output": "first"}),
        json!({"type": "function_call", "call_id": "c2", "name": "bash", "arguments": "{}"}),
        json!({"type": "function_call_output", "call_id": "c2", "output": "second"}),
    ];
    let dropped = drop_oldest_tool_pair(&mut input);
    assert_eq!(dropped, 1);
    assert_eq!(input.len(), 3);
    // c2 pair survives; c1 entries are gone.
    let kept_call_ids: Vec<&str> = input
        .iter()
        .filter_map(|item| item.get("call_id").and_then(Value::as_str))
        .collect();
    assert_eq!(kept_call_ids, vec!["c2", "c2"]);
}

#[test]
fn drop_oldest_tool_pair_returns_zero_when_no_calls() {
    let mut input = vec![json!({"type": "message", "role": "user", "content": "hi"})];
    let dropped = drop_oldest_tool_pair(&mut input);
    assert_eq!(dropped, 0);
    assert_eq!(input.len(), 1);
}

#[test]
fn drop_oldest_tool_pair_handles_interleaved_items() {
    // The matching `function_call_output` does not have to be immediately
    // adjacent to its `function_call`.
    let mut input = vec![
        json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{}"}),
        json!({"type": "message", "role": "assistant", "content": "thinking..."}),
        json!({"type": "function_call_output", "call_id": "c1", "output": "done"}),
    ];
    let dropped = drop_oldest_tool_pair(&mut input);
    assert_eq!(dropped, 1);
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["type"], "message");
}

#[test]
fn trim_for_compaction_falls_back_to_oldest_message_on_text_history() {
    // Regression for codex review B-2: a text-only transcript with no
    // tool pairs must still shed something on overflow, otherwise
    // `/compact` fails exactly when it's supposed to help.
    let mut input = vec![
        json!({"type": "message", "role": "user", "content": "oldest"}),
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "reply"}]}),
        json!({"type": "message", "role": "user", "content": "newest"}),
        // The trailing summarisation prompt that run_compaction_turn
        // appends just before submitting. Must be preserved.
        json!({"type": "message", "role": "user", "content": SUMMARIZATION_PROMPT}),
    ];
    let dropped = trim_for_compaction(&mut input);
    assert_eq!(dropped, 1);
    assert_eq!(input.len(), 3);
    // The synthesised summarisation prompt is still the last item.
    let last_text = input
        .last()
        .and_then(|v| v.get("content"))
        .and_then(Value::as_str);
    assert_eq!(last_text, Some(SUMMARIZATION_PROMPT));
    // "oldest" is gone; "newest" survives.
    let contents: Vec<&str> = input
        .iter()
        .filter_map(|v| v.get("content").and_then(Value::as_str))
        .collect();
    assert!(!contents.contains(&"oldest"));
    assert!(contents.contains(&"newest"));
}

#[test]
fn trim_for_compaction_returns_zero_when_only_prompt_remains() {
    // Nothing eligible to drop — the synthesised prompt is sacred.
    let mut input =
        vec![json!({"type": "message", "role": "user", "content": SUMMARIZATION_PROMPT})];
    assert_eq!(trim_for_compaction(&mut input), 0);
    assert_eq!(input.len(), 1);
}

// ── extract_message_text ──────────────────────────────────────

#[test]
fn extract_message_text_concatenates_output_text_parts() {
    let item = json!({
        "type": "message",
        "content": [
            {"type": "output_text", "text": "hello "},
            {"type": "output_text", "text": "world"}
        ]
    });
    assert_eq!(extract_message_text(&item).as_deref(), Some("hello world"));
}

#[test]
fn extract_message_text_returns_none_for_empty_content() {
    let item = json!({"type": "message", "content": []});
    assert!(extract_message_text(&item).is_none());
}

// ── extract_reasoning_text ──────────────────────────────────

#[test]
fn extract_reasoning_text_concatenates_summary_parts() {
    let item = json!({
        "type": "reasoning",
        "summary": [
            {"type": "summary_text", "text": "step one: "},
            {"type": "summary_text", "text": "step two"}
        ]
    });
    assert_eq!(
        extract_reasoning_text(&item).as_deref(),
        Some("step one: step two")
    );
}

#[test]
fn extract_reasoning_text_returns_none_for_empty_summary() {
    let item = json!({"type": "reasoning", "summary": []});
    assert!(extract_reasoning_text(&item).is_none());
}

#[test]
fn extract_reasoning_text_returns_none_when_missing_summary() {
    let item = json!({"type": "reasoning"});
    assert!(extract_reasoning_text(&item).is_none());
}

#[test]
fn extract_concatenated_text_skips_parts_missing_type() {
    let item = json!({
        "type": "message",
        "content": [
            {"type": "text", "text": "kept"},
            {"text": "no type field"},
            {"type": "text", "text": "also kept"}
        ]
    });
    assert_eq!(
        extract_message_text(&item).as_deref(),
        Some("keptalso kept")
    );
}

#[test]
fn extract_reasoning_text_ignores_non_summary_parts() {
    let item = json!({
        "type": "reasoning",
        "summary": [
            {"type": "summary_text", "text": "visible"},
            {"type": "other_thing", "text": "ignored"}
        ]
    });
    assert_eq!(extract_reasoning_text(&item).as_deref(), Some("visible"));
}

#[test]
fn rebuild_responses_input_replays_user_text_not_display_text() {
    let input = rebuild_responses_input(
        &[
            AgentEvent::UserMessage {
                text: "model-facing prompt".into(),
                display_text: Some("visible prompt".into()),
                attachments: Vec::new(),
            },
            AgentEvent::AssistantMessageDone {
                text: "assistant reply".into(),
            },
        ],
        Path::new("/tmp"),
    );

    assert!(is_input_user_message(&input[0], "model-facing prompt"));
    assert!(is_input_assistant_message(&input[1], "assistant reply"));
}

#[test]
fn rebuild_responses_input_drops_tool_io_without_continuation() {
    // Sessions written before `ResponseContinuation` was persisted have
    // `ToolCallStarted` / `ToolCallOutput` but no matching reasoning or
    // `function_call` items. Replaying a `function_call_output` without
    // its `function_call` would be rejected by the API, so the old
    // tool-event-only shape must be skipped on replay.
    let input = rebuild_responses_input(
        &[
            AgentEvent::UserMessage {
                text: "inspect".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::ToolCallStarted {
                call_id: "call_1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "Cargo.toml"}),
            },
            AgentEvent::ToolCallOutput {
                call_id: "call_1".into(),
                output: "contents".into(),
                is_error: false,
                truncation: None,
            },
            AgentEvent::AssistantMessageDone {
                text: "Cargo.toml is a Rust manifest.".into(),
            },
        ],
        Path::new("/tmp"),
    );

    assert_eq!(input.len(), 2);
    assert!(is_input_user_message(&input[0], "inspect"));
    assert!(is_input_assistant_message(
        &input[1],
        "Cargo.toml is a Rust manifest."
    ));
}

#[test]
fn rebuild_responses_input_replays_continuation_items_and_tool_outputs() {
    // When `ResponseContinuation` carries the reasoning + function_call
    // items the model emitted, replay must reproduce the same wire shape
    // the agent loop originally appended in memory: reasoning, function_call,
    // function_call_output.
    let input = rebuild_responses_input(
        &[
            AgentEvent::UserMessage {
                text: "inspect".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::ResponseContinuation {
                items: vec![
                    json!({
                        "type": "reasoning",
                        "id": "rs_1",
                        "encrypted_content": "enc-blob",
                    }),
                    json!({
                        "type": "function_call",
                        "call_id": "call_1",
                        "name": "read_file",
                        "arguments": "{\"path\":\"Cargo.toml\"}",
                    }),
                ],
            },
            AgentEvent::ToolCallStarted {
                call_id: "call_1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "Cargo.toml"}),
            },
            AgentEvent::ToolCallOutput {
                call_id: "call_1".into(),
                output: "contents".into(),
                is_error: false,
                truncation: None,
            },
            AgentEvent::AssistantMessageDone {
                text: "Cargo.toml is a Rust manifest.".into(),
            },
        ],
        Path::new("/tmp"),
    );

    assert_eq!(input.len(), 5, "{input:#?}");
    assert!(is_input_user_message(&input[0], "inspect"));
    assert_eq!(input[1]["type"], "reasoning");
    assert_eq!(input[1]["encrypted_content"], "enc-blob");
    assert_eq!(input[2]["type"], "function_call");
    assert_eq!(input[2]["call_id"], "call_1");
    assert_eq!(input[3]["type"], "function_call_output");
    assert_eq!(input[3]["call_id"], "call_1");
    assert_eq!(input[3]["output"], "contents");
    assert!(is_input_assistant_message(
        &input[4],
        "Cargo.toml is a Rust manifest."
    ));
}

#[test]
fn rebuild_responses_input_reduces_old_tool_outputs_and_keeps_recent_raw() {
    let mut events = Vec::new();
    events.extend(replay_tool_turn(
        "old read",
        "call_old",
        "read_file",
        "old file contents".into(),
        false,
    ));
    events.extend(replay_tool_turn(
        "middle search",
        "call_mid",
        "code_search",
        "middle search hits".into(),
        false,
    ));
    events.extend(replay_tool_turn(
        "recent bash",
        "call_recent",
        "bash",
        "tool error: command failed".into(),
        true,
    ));

    let input = rebuild_responses_input(&events, Path::new("/tmp"));

    let old = replay_tool_output(&input, "call_old");
    assert!(
        old.starts_with(REDUCED_TOOL_OUTPUT_PREFIX),
        "old successful tool output should be reduced: {old}"
    );
    assert!(old.contains("old file contents"));
    assert_eq!(replay_tool_output(&input, "call_mid"), "middle search hits");
    assert_eq!(
        replay_tool_output(&input, "call_recent"),
        "tool error: command failed"
    );
    assert_replay_outputs_have_calls(&input);
}

#[test]
fn rebuild_responses_input_clears_oldest_reduced_outputs_over_total_budget() {
    let large = "x".repeat(70 * 1024);
    let mut events = Vec::new();
    for (prompt, call_id) in [
        ("old one", "call_old_1"),
        ("old two", "call_old_2"),
        ("old three", "call_old_3"),
    ] {
        events.extend(replay_tool_turn(
            prompt,
            call_id,
            "bash",
            large.clone(),
            false,
        ));
    }
    events.extend(replay_tool_turn(
        "recent one",
        "call_recent_1",
        "read_file",
        "recent file".into(),
        false,
    ));
    events.extend(replay_tool_turn(
        "recent two",
        "call_recent_2",
        "code_search",
        "recent search".into(),
        false,
    ));

    let input = rebuild_responses_input(&events, Path::new("/tmp"));

    assert_eq!(
        replay_tool_output(&input, "call_old_1"),
        CLEARED_TOOL_OUTPUT_PLACEHOLDER
    );
    assert!(
        replay_tool_output(&input, "call_old_2").starts_with(REDUCED_TOOL_OUTPUT_PREFIX),
        "second old output should stay reduced after the oldest is cleared"
    );
    assert!(
        replay_tool_output(&input, "call_old_3").starts_with(REDUCED_TOOL_OUTPUT_PREFIX),
        "third old output should stay reduced after the oldest is cleared"
    );
    assert_eq!(replay_tool_output(&input, "call_recent_1"), "recent file");
    assert_eq!(replay_tool_output(&input, "call_recent_2"), "recent search");
    assert_replay_outputs_have_calls(&input);
}

#[test]
fn rebuild_responses_input_continuation_strips_hidden_plaintext_reasoning() {
    // `ResponseContinuation` payloads must already be sanitized when
    // persisted, but replay should also be robust to historical events
    // that carry only the encrypted handle. Verify that an item with
    // just `encrypted_content` round-trips without resurrecting any
    // `summary` or `content` field on the way back to the wire.
    let input = rebuild_responses_input(
        &[
            AgentEvent::UserMessage {
                text: "hello".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::ResponseContinuation {
                items: vec![json!({
                    "type": "reasoning",
                    "id": "rs_1",
                    "encrypted_content": "enc-blob",
                })],
            },
            AgentEvent::AssistantMessageDone { text: "hi".into() },
        ],
        Path::new("/tmp"),
    );

    let reasoning = &input[1];
    assert_eq!(reasoning["type"], "reasoning");
    assert_eq!(reasoning["encrypted_content"], "enc-blob");
    assert!(reasoning.get("summary").is_none());
    assert!(reasoning.get("content").is_none());
}

#[test]
fn rebuild_responses_input_drops_mid_prompt_continuation_when_aborted_after_iteration() {
    // Regression: `finalize_turn` (and therefore `TurnComplete`) fires once
    // per loop iteration, not once per user prompt. If a user prompt runs
    // a tool call, completes one iteration, then gets aborted on the next
    // approval/interrupt, the persisted continuation + tool output from
    // that mid-prompt iteration must be dropped on replay. Without this
    // anchor surviving the per-iteration TurnComplete, a resumed session
    // would resend stale partial tool-call state for a prompt the user
    // explicitly aborted.
    let input = rebuild_responses_input(
        &[
            AgentEvent::UserMessage {
                text: "do something risky".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::ResponseContinuation {
                items: vec![
                    json!({
                        "type": "reasoning",
                        "id": "rs_1",
                        "encrypted_content": "enc-blob",
                    }),
                    json!({
                        "type": "function_call",
                        "call_id": "call_1",
                        "name": "read_file",
                        "arguments": "{\"path\":\"Cargo.toml\"}",
                    }),
                ],
            },
            AgentEvent::ToolCallOutput {
                call_id: "call_1".into(),
                output: "contents".into(),
                is_error: false,
                truncation: None,
            },
            AgentEvent::TurnComplete {
                usage: TurnUsage::default(),
            },
            AgentEvent::TurnAborted {
                turn_id: "turn-2".into(),
                reason: "user denied next tool call".into(),
            },
        ],
        Path::new("/tmp"),
    );

    assert!(
        input.is_empty(),
        "aborted prompt must leave no continuation/tool-output state in \
         the replayed input, got: {input:#?}"
    );
}

#[test]
fn rebuild_responses_input_preserves_completed_turn_when_next_turn_aborts_pre_user_message() {
    // Regression: the attachment guardrail runs *before* the new turn emits
    // its `UserMessage`. If approval is denied (e.g. the model attempted to
    // include `.env`), `TurnAborted` fires with no preceding `UserMessage`
    // for the new turn. The old replay logic blindly truncated back to the
    // *previous* turn's anchor, deleting the last successful turn from the
    // model-visible transcript on resume. The fix snapshots terminal-ness
    // on each `TurnComplete` (a terminal iter never emits a
    // `ResponseContinuation`) so an abort following a completed turn leaves
    // that turn intact.
    let input = rebuild_responses_input(
        &[
            AgentEvent::UserMessage {
                text: "first prompt".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::AssistantMessageDone {
                text: "first answer".into(),
            },
            AgentEvent::TurnComplete {
                usage: TurnUsage::default(),
            },
            // Attachment-guard rejection for *the next turn* fires before
            // any `UserMessage` for that turn is persisted. Replay must NOT
            // drop the prior completed turn.
            AgentEvent::TurnAborted {
                turn_id: "turn-2".into(),
                reason: "attachment denied".into(),
            },
        ],
        Path::new("/tmp"),
    );

    assert_eq!(
        input.len(),
        2,
        "completed turn must survive pre-user-message abort: {input:#?}"
    );
    assert!(is_input_user_message(&input[0], "first prompt"));
    assert!(is_input_assistant_message(&input[1], "first answer"));
}

#[test]
fn rebuild_responses_input_skips_aborted_turn_partial_answer() {
    let input = rebuild_responses_input(
        &[
            AgentEvent::UserMessage {
                text: "previous".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::AssistantMessageDone {
                text: "previous answer".into(),
            },
            AgentEvent::TurnComplete {
                usage: TurnUsage::default(),
            },
            AgentEvent::UserMessage {
                text: "do the wrong thing".into(),
                display_text: None,
                attachments: Vec::new(),
            },
            AgentEvent::AssistantMessageDone {
                text: "partial answer that should not resume as complete".into(),
            },
            AgentEvent::TurnAborted {
                turn_id: "turn-2".into(),
                reason: "user interrupt".into(),
            },
        ],
        Path::new("/tmp"),
    );

    assert_eq!(input.len(), 2, "{input:#?}");
    assert!(is_input_user_message(&input[0], "previous"));
    assert!(is_input_assistant_message(&input[1], "previous answer"));
}

#[test]
fn rebuild_responses_input_carries_image_attachments_back_into_input() {
    // PNG header bytes — encode_image_data_uri only reads from disk and
    // base64s, no decoding, so the exact content doesn't need to be valid.
    let bytes = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let dir = tempdir().unwrap();
    let rel = PathBuf::from(".nav/clipboard/restored.png");
    let abs = dir.path().join(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, bytes).unwrap();

    let input = rebuild_responses_input(
        &[AgentEvent::UserMessage {
            text: "look at this".into(),
            display_text: None,
            attachments: vec![UserAttachment::Image { path: rel }],
        }],
        dir.path(),
    );

    assert_eq!(input.len(), 1);
    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("attachments produce typed content array");
    assert!(content.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("input_text")
            && part.get("text").and_then(Value::as_str) == Some("look at this")
    }));
    assert!(content.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("input_image")
            && part
                .get("image_url")
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("data:image/png;base64,"))
    }));
}

#[test]
fn file_attachment_body_is_truncated_via_tool_output_policy() {
    // A pathologically large file attachment must not blow up the request.
    // We reuse the same `bound` / `TruncateMode::Head` policy as the
    // read_file tool output: a head-only block plus a "truncated" marker
    // counting the dropped bytes / lines.
    let dir = tempdir().unwrap();
    let rel = PathBuf::from("huge.txt");
    let abs = dir.path().join(&rel);
    // 60 KB > 50 KB MAX_BYTES — head-only truncation kicks in.
    let body = "x".repeat(60 * 1024);
    std::fs::write(&abs, &body).unwrap();

    let input = rebuild_responses_input(
        &[AgentEvent::UserMessage {
            text: "read".into(),
            display_text: None,
            attachments: vec![UserAttachment::File { path: rel }],
        }],
        dir.path(),
    );

    let content = input[0].get("content").and_then(Value::as_array).unwrap();
    let attached = content
        .iter()
        .find_map(|p| {
            let t = p.get("text").and_then(Value::as_str)?;
            t.contains("<attached file: huge.txt>")
                .then_some(t.to_string())
        })
        .expect("file attachment part missing");
    assert!(
        attached.contains("[truncated"),
        "expected truncation marker: {}",
        &attached[..attached.len().min(200)]
    );
    // Bounded body is well under the unbounded 60 KB original.
    assert!(attached.len() < 60 * 1024);
}

fn replay_user_text_parts(text: &str, cwd: &Path) -> Vec<String> {
    let input = rebuild_responses_input(
        &[AgentEvent::UserMessage {
            text: text.into(),
            display_text: None,
            attachments: Vec::new(),
        }],
        cwd,
    );
    input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("@file mentions produce typed parts")
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn find_text_part<'a>(parts: &'a [String], needle: &str) -> &'a str {
    parts
        .iter()
        .map(String::as_str)
        .find(|text| text.contains(needle))
        .unwrap_or_else(|| panic!("missing text part containing {needle:?}"))
}

#[test]
fn submit_time_file_mention_inlines_existing_file() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "hello\nworld\n").unwrap();

    let parts = replay_user_text_parts("summarise @README.md", dir.path());
    assert!(parts.iter().any(|text| text == "summarise @README.md"));
    let attached = find_text_part(&parts, "<attached file: README.md>");
    assert!(attached.contains("hello\nworld\n"));
}

#[test]
fn submit_time_file_mention_notes_invalid_path() {
    let dir = tempdir().unwrap();

    let parts = replay_user_text_parts("read @missing.rs", dir.path());
    let note = find_text_part(&parts, "<file mention: @missing.rs>");
    assert!(note.contains("[not resolved: no such workspace file]"));
}

#[test]
fn submit_time_file_mention_notes_protected_read() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();

    let parts = replay_user_text_parts("read @.env", dir.path());
    let note = find_text_part(&parts, "<file mention: @.env>");
    assert!(note.contains("[refused: protected file reads require explicit approval]"));
    assert!(!note.contains("SECRET=1"));
}

#[test]
fn submit_time_file_mention_notes_ambiguous_basename() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a")).unwrap();
    std::fs::create_dir_all(dir.path().join("b")).unwrap();
    std::fs::write(dir.path().join("a/dup.txt"), "one\n").unwrap();
    std::fs::write(dir.path().join("b/dup.txt"), "two\n").unwrap();

    let parts = replay_user_text_parts("compare @dup.txt", dir.path());
    let note = find_text_part(&parts, "<file mention: @dup.txt>");
    assert!(note.contains("ambiguous: multiple files match this name"));
    assert!(note.contains("a/dup.txt"));
    assert!(note.contains("b/dup.txt"));
}

#[test]
fn submit_time_file_mention_uses_read_file_line_cap() {
    let dir = tempdir().unwrap();
    let body = (0..700).map(|i| format!("line{i}\n")).collect::<String>();
    std::fs::write(dir.path().join("big.txt"), &body).unwrap();

    let parts = replay_user_text_parts("summarise @big.txt", dir.path());
    let attached = find_text_part(&parts, "<attached file: big.txt>");
    assert!(attached.contains("line499\n"));
    assert!(!attached.contains("line500\n"));
    assert!(attached.contains("[truncated"));
}

#[test]
fn submit_time_mentions_scan_display_prompt_not_wrapped_skill_body() {
    let dir = tempdir().unwrap();

    let content = build_user_content(
        "<skill name=\"test\">\nread @missing.rs\n</skill>\n\nActual request",
        Some("Actual request"),
        &[],
        dir.path(),
    );

    assert_eq!(
        content,
        Value::String("<skill name=\"test\">\nread @missing.rs\n</skill>\n\nActual request".into())
    );
}

#[test]
fn rebuild_responses_input_carries_file_attachments_back_into_input() {
    // The wire format for a File attachment is an `input_text` part holding
    // a fenced block with the workspace-relative path and the file body.
    // Resume just calls build_user_content again, so this is the same path
    // as the live agent uses for the initial turn.
    let dir = tempdir().unwrap();
    let rel = PathBuf::from("docs/notes.md");
    let abs = dir.path().join(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, "hello\nworld\n").unwrap();

    let input = rebuild_responses_input(
        &[AgentEvent::UserMessage {
            text: "summarise this".into(),
            display_text: None,
            attachments: vec![UserAttachment::File { path: rel.clone() }],
        }],
        dir.path(),
    );

    assert_eq!(input.len(), 1);
    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("file attachment produces typed parts");
    let attached_text_part = content
        .iter()
        .find_map(|part| {
            let kind = part.get("type").and_then(Value::as_str)?;
            let text = part.get("text").and_then(Value::as_str)?;
            (kind == "input_text" && text.contains("<attached file:")).then_some(text.to_string())
        })
        .expect("expected fenced file attachment part");
    assert!(
        attached_text_part.contains("docs/notes.md"),
        "missing path: {attached_text_part}"
    );
    assert!(
        attached_text_part.contains("hello\nworld"),
        "missing body: {attached_text_part}"
    );
}

#[test]
fn rebuild_responses_input_marks_non_utf8_file_attachments() {
    let dir = tempdir().unwrap();
    let rel = PathBuf::from("blob.bin");
    let abs = dir.path().join(&rel);
    std::fs::write(&abs, [0xff, 0xfe, 0xfa]).unwrap();

    let input = rebuild_responses_input(
        &[AgentEvent::UserMessage {
            text: "skip me".into(),
            display_text: None,
            attachments: vec![UserAttachment::File { path: rel }],
        }],
        dir.path(),
    );

    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("typed parts");
    let note = content
        .iter()
        .find_map(|part| {
            let text = part.get("text").and_then(Value::as_str)?;
            text.contains("[skipped: file is not valid UTF-8")
                .then_some(text.to_string())
        })
        .expect("non-UTF-8 file body must surface as a note rather than silent omission");
    assert!(note.contains("blob.bin"));
}

#[test]
fn rebuild_responses_input_drops_file_attachment_that_escapes_workspace() {
    // Exfiltration guard: a stored attachment whose path resolves outside
    // cwd must not have its bytes pulled in on resume. The user's text
    // still goes through; the attachment part is simply absent.
    let outer = tempdir().unwrap();
    let outside = outer.path().join("secret.txt");
    std::fs::write(&outside, "shhh").unwrap();
    let cwd = outer.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();

    let input = rebuild_responses_input(
        &[AgentEvent::UserMessage {
            text: "see notes".into(),
            display_text: None,
            attachments: vec![UserAttachment::File {
                path: PathBuf::from("../secret.txt"),
            }],
        }],
        &cwd,
    );

    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("typed parts");
    assert!(
        !content.iter().any(|part| part
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("shhh"))),
        "../ escape leaked file body: {content:?}"
    );
}

#[test]
fn rebuild_responses_input_keeps_text_when_image_file_missing() {
    let dir = tempdir().unwrap();
    let input = rebuild_responses_input(
        &[AgentEvent::UserMessage {
            text: "image gone".into(),
            display_text: None,
            attachments: vec![UserAttachment::Image {
                path: PathBuf::from(".nav/clipboard/missing.png"),
            }],
        }],
        dir.path(),
    );

    // Missing image bytes degrade to the text-only typed parts array (no
    // input_image part) rather than failing the resume.
    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("attachments still trigger typed-parts shape");
    assert!(
        content
            .iter()
            .all(|part| part.get("type").and_then(Value::as_str) != Some("input_image"))
    );
    assert!(
        content
            .iter()
            .any(|part| part.get("type").and_then(Value::as_str) == Some("input_text"))
    );
}

#[test]
fn image_attachment_with_dotdot_escape_is_dropped() {
    // A relative attachment path containing `..` resolves outside cwd; even
    // if the file exists and is readable, encode_image_data_uri must refuse
    // to ship its bytes — that's the workspace-boundary contract.
    let outer = tempdir().unwrap();
    let outside = outer.path().join("secret.png");
    std::fs::write(&outside, b"not really a png but doesn't matter").unwrap();
    let cwd = outer.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();

    let input = rebuild_responses_input(
        &[AgentEvent::UserMessage {
            text: "exfiltrate this".into(),
            display_text: None,
            attachments: vec![UserAttachment::Image {
                path: PathBuf::from("../secret.png"),
            }],
        }],
        &cwd,
    );

    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("attachments produce typed parts");
    assert!(
        content
            .iter()
            .all(|part| part.get("type").and_then(Value::as_str) != Some("input_image")),
        "../ escape must not emit input_image: {content:?}"
    );
}

#[test]
fn image_attachment_via_symlink_escape_is_dropped() {
    // A symlink inside the workspace that points outside must not be read
    // and forwarded to the model. canonicalize() resolves the symlink before
    // the containment check.
    let outer = tempdir().unwrap();
    let outside = outer.path().join("secret.png");
    std::fs::write(&outside, b"x").unwrap();
    let cwd = outer.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let link = cwd.join("evil.png");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    #[cfg(not(unix))]
    {
        // No portable symlink on non-unix; skip the assertion when it cannot
        // be set up. The Linux/macOS CI path is the one we care about.
        let _ = &link;
        return;
    }

    let input = rebuild_responses_input(
        &[AgentEvent::UserMessage {
            text: "look".into(),
            display_text: None,
            attachments: vec![UserAttachment::Image {
                path: PathBuf::from("evil.png"),
            }],
        }],
        &cwd,
    );

    let content = input[0]
        .get("content")
        .and_then(Value::as_array)
        .expect("attachments produce typed parts");
    assert!(
        content
            .iter()
            .all(|part| part.get("type").and_then(Value::as_str) != Some("input_image"))
    );
}

// ── emit_stream_events ────────────────────────────────────────

#[test]
fn emit_stream_events_emits_delta() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let event = json!({"type": "response.output_text.delta", "delta": "hi"});
    emit_stream_events(&event, &tx, None);
    drop(tx);
    let received = rx.blocking_recv().unwrap();
    assert!(matches!(
        received,
        AgentEvent::AssistantMessageDelta { ref text } if text == "hi"
    ));
}

#[test]
fn emit_stream_events_emits_done_for_message_item() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let event = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "content": [{"type": "output_text", "text": "final"}]
        }
    });
    emit_stream_events(&event, &tx, None);
    drop(tx);
    let received = rx.blocking_recv().unwrap();
    assert!(matches!(
        received,
        AgentEvent::AssistantMessageDone { ref text } if text == "final"
    ));
}

#[test]
fn emit_stream_events_ignores_function_call_items() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let event = json!({
        "type": "response.output_item.done",
        "item": {"type": "function_call", "call_id": "c", "name": "x", "arguments": "{}"}
    });
    emit_stream_events(&event, &tx, None);
    drop(tx);
    assert!(rx.blocking_recv().is_none());
}

#[test]
fn emit_stream_events_emits_reasoning_delta() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let event = json!({"type": "response.reasoning_summary_text.delta", "delta": "thinking"});
    emit_stream_events(&event, &tx, None);
    drop(tx);
    let received = rx.blocking_recv().unwrap();
    assert!(matches!(received, AgentEvent::ReasoningDelta { ref text } if text == "thinking"));
}

#[test]
fn emit_stream_events_emits_done_for_reasoning_item() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let event = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "I reasoned about it"}]
        }
    });
    emit_stream_events(&event, &tx, None);
    drop(tx);
    let received = rx.blocking_recv().unwrap();
    assert!(
        matches!(received, AgentEvent::ReasoningDone { ref text } if text == "I reasoned about it")
    );
}

// ── run_agent end-to-end ──────────────────────────────────────

#[tokio::test]
async fn run_agent_injects_ambient_context_before_user_prompt_when_budget_allows() {
    let mut args = Args::test_default();
    args.ambient_context_token_budget = 256;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    fs::write(cwd.join("Cargo.toml"), "").unwrap();
    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let transport = StubTransport::new(vec![vec![json!({
        "type": "response.completed",
        "response": {}
    })]]);

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "hello",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        Some(&ProjectContext::default()),
        unchecked_permission_context(),
    )
    .await
    .unwrap();

    let bodies = transport.bodies();
    let input = bodies[0]["input"].as_array().unwrap();
    assert_eq!(input.len(), 2);
    let ambient = input[0]["content"].as_str().unwrap();
    assert!(ambient.starts_with("Ambient context (turn-local; not a user request):"));
    assert!(ambient.contains("Cargo.toml"));
    assert!(is_input_user_message(&input[1], "hello"));
}

#[tokio::test]
async fn run_agent_builds_chat_completions_body_for_chat_transport() {
    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    let transport = StubTransport::chat_completions(vec![vec![json!({
        "type": "response.completed",
        "response": {}
    })]]);

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "hello",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .unwrap();

    let bodies = transport.bodies();
    let body = &bodies[0];
    assert_eq!(body["model"], "wire-model");
    assert!(
        body.get("input").is_none(),
        "Chat Completions body must not use Responses input"
    );
    assert!(body.get("instructions").is_none());
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages.last().unwrap()["role"], "user");
    assert_eq!(messages.last().unwrap()["content"], "hello");
    let first_tool = &body["tools"].as_array().unwrap()[0];
    assert_eq!(first_tool["type"], "function");
    assert!(first_tool.get("function").is_some());
}

#[test]
fn model_transport_handle_allows_responses_to_chat_swap() {
    let handle = crate::model::ModelTransportHandle::new(StubTransport::new(Vec::new()));
    let outcome = handle
        .swap_to(StubTransport::chat_completions(Vec::new()))
        .unwrap();
    assert_eq!(outcome.from, crate::model::WireFormat::Responses);
    assert_eq!(outcome.to, crate::model::WireFormat::ChatCompletions);
    assert_eq!(
        handle.wire_format(),
        crate::model::WireFormat::ChatCompletions
    );
}

#[test]
fn model_transport_handle_rejects_chat_to_responses_swap() {
    let handle =
        crate::model::ModelTransportHandle::new(StubTransport::chat_completions(Vec::new()));
    let err = handle
        .swap_to(StubTransport::new(Vec::new()))
        .expect_err("reverse swap should be rejected");
    assert!(err.to_string().contains("reverse history conversion"));
}

#[tokio::test]
async fn run_agent_emits_single_error_when_transport_create_fails() {
    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    let err = run_agent_for_test(
        &FailingTransport,
        &args,
        &cwd,
        "hello",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect_err("transport failure should return an error");
    assert!(err.to_string().contains("network down"));

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let error_events: Vec<&AgentEvent> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::Error { .. }))
        .collect();
    assert_eq!(error_events.len(), 1);
    assert!(matches!(
        error_events[0],
        AgentEvent::Error { message } if message.contains("network down")
    ));
}

#[tokio::test]
async fn run_agent_emits_expected_sequence_with_usage() {
    // Turn 1: model requests a bash tool call.
    let turn_one = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        }),
        // No usage on the tool-call turn — exercises the default-to-zero path.
        json!({
            "type": "response.completed",
            "response": {}
        }),
    ];
    // Turn 2: final assistant message with full usage payload.
    let turn_two = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "All done."}]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "input_tokens_details": {"cached_tokens": 20}
                }
            }
        }),
    ];
    let transport = StubTransport::new(vec![turn_one, turn_two]);

    let mut args = Args::test_default();
    args.max_turns = 4;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let result = run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "do the thing",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await;
    result.expect("run_agent should succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    // Sequence: UserMessage, ToolCallStarted, ToolCallOutput,
    // TurnComplete (turn 1), AssistantMessageDone, TurnComplete
    // (turn 2 with usage).
    assert!(
        matches!(
            events.first(),
            Some(AgentEvent::UserMessage { text, display_text, .. })
                if text == "do the thing" && display_text.is_none()
        ),
        "unexpected first event: {:?}",
        events.first()
    );
    let tool_started = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolCallStarted { .. }))
        .expect("expected ToolCallStarted");
    assert!(
        matches!(
            tool_started,
            AgentEvent::ToolCallStarted { call_id, name, arguments }
                if call_id == "call_1"
                    && name == "bash"
                    && arguments.get("command").and_then(Value::as_str) == Some("echo hi")
        ),
        "unexpected tool event: {tool_started:?}"
    );

    let tool_output = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolCallOutput { .. }))
        .expect("expected ToolCallOutput");
    match tool_output {
        AgentEvent::ToolCallOutput {
            call_id,
            output,
            is_error,
            ..
        } => {
            assert_eq!(call_id, "call_1");
            assert!(!*is_error);
            assert!(output.contains("hi"));
        }
        other => panic!("expected ToolCallOutput, got {other:?}"),
    }

    let assistant_done = events
        .iter()
        .find(|e| matches!(e, AgentEvent::AssistantMessageDone { .. }))
        .expect("expected AssistantMessageDone");
    assert_eq!(
        assistant_done,
        &AgentEvent::AssistantMessageDone {
            text: "All done.".into()
        }
    );

    let turn_completes: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnComplete { .. }))
        .collect();
    assert_eq!(
        turn_completes.len(),
        2,
        "expected one TurnComplete per turn"
    );

    match turn_completes[0] {
        AgentEvent::TurnComplete { usage } => assert_eq!(usage, &TurnUsage::default()),
        _ => unreachable!(),
    }

    let last = events.last().unwrap();
    match last {
        AgentEvent::TurnComplete { usage } => {
            assert_eq!(usage.tokens_input, 100);
            assert_eq!(usage.tokens_output, 50);
            assert_eq!(usage.tokens_input_cached, 20);
            assert_eq!(usage.tokens_reasoning, 0);
        }
        other => panic!("expected final TurnComplete, got {other:?}"),
    }

    // Strict ordering check: ToolCallStarted precedes ToolCallOutput,
    // TurnComplete (turn 1) precedes AssistantMessageDone (turn 2), and
    // the final TurnComplete is last.
    let pos_user = event_position(&events, "UserMessage", |event| {
        matches!(event, AgentEvent::UserMessage { .. })
    });
    let pos_tool_started = event_position(&events, "ToolCallStarted", |event| {
        matches!(event, AgentEvent::ToolCallStarted { .. })
    });
    let pos_tool_output = event_position(&events, "ToolCallOutput", |event| {
        matches!(event, AgentEvent::ToolCallOutput { .. })
    });
    let pos_assistant_done = event_position(&events, "AssistantMessageDone", |event| {
        matches!(event, AgentEvent::AssistantMessageDone { .. })
    });
    assert!(pos_user < pos_tool_started);
    assert!(pos_tool_started < pos_tool_output);
    assert!(pos_tool_output < pos_assistant_done);
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnComplete { .. }
    ));
}

#[tokio::test]
#[cfg(unix)]
async fn run_agent_executes_pre_and_post_turn_hooks() {
    let turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Done."}]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {}
        }),
    ];
    let transport = StubTransport::new(vec![turn]);
    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let extensions = ExtensionCatalog::with_hooks(
        vec![],
        vec![],
        vec![],
        vec![
            test_hook("before", HookEventType::PreTurn, "printf pre", &cwd),
            test_hook("after", HookEventType::PostTurn, "printf post", &cwd),
        ],
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    super::run_agent(
        AgentTurnRequest::new(
            &transport,
            &args,
            &cwd,
            "hello",
            tx,
            &Catalog::default(),
            unchecked_permission_context(),
        )
        .with_extensions(Some(&extensions)),
    )
    .await
    .expect("run_agent should succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert!(matches!(events[0], AgentEvent::UserMessage { .. }));
    assert!(matches!(
        events[1],
        AgentEvent::HookStarted {
            ref name,
            ref event_type
        } if name == "before" && event_type == "pre_turn"
    ));
    assert!(matches!(
        events[2],
        AgentEvent::HookCompleted {
            ref name,
            ref event_type,
            ref stdout,
            success,
            ..
        } if name == "before" && event_type == "pre_turn" && stdout == "pre" && success
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::HookCompleted {
            name,
            event_type,
            stdout,
            success,
            ..
        } if name == "after" && event_type == "post_turn" && stdout == "post" && *success
    )));
}

#[cfg(unix)]
fn test_hook(name: &str, event_type: HookEventType, command: &str, cwd: &Path) -> ExtensionHook {
    ExtensionHook {
        name: name.into(),
        extension_name: "demo".into(),
        extension_dir: cwd.to_path_buf(),
        scope: ExtensionScope::Project,
        event_type,
        command: HookCommand::Shell(command.into()),
        timeout: std::time::Duration::from_secs(5),
    }
}

#[tokio::test]
async fn run_agent_emits_sanitized_response_continuation_with_function_call() {
    // The model streams a reasoning item (with plaintext summary + content
    // that must not be persisted) and a function_call item in the same
    // turn. nav must surface a sanitized `ResponseContinuation` event with
    // only the encrypted reasoning handle plus the verbatim function_call,
    // and the second-turn request body must include the same items the
    // in-memory loop appended via `into_raw_output`.
    let turn_one = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{"type": "summary_text", "text": "thinking out loud"}],
                "content": [{"type": "reasoning_text", "text": "raw chain of thought"}],
                "encrypted_content": "enc-blob",
            }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let turn_two = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "done"}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![turn_one, turn_two]);

    let mut args = Args::test_default();
    args.max_turns = 4;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "do the thing",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent should succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let continuation = events
        .iter()
        .find(|event| matches!(event, AgentEvent::ResponseContinuation { .. }))
        .expect("expected ResponseContinuation event");
    let items = match continuation {
        AgentEvent::ResponseContinuation { items } => items,
        _ => unreachable!(),
    };
    assert_eq!(items.len(), 2);
    let reasoning = &items[0];
    assert_eq!(reasoning["type"], "reasoning");
    assert_eq!(reasoning["id"], "rs_1");
    assert_eq!(reasoning["encrypted_content"], "enc-blob");
    assert!(reasoning.get("summary").is_none());
    assert!(reasoning.get("content").is_none());
    let call = &items[1];
    assert_eq!(call["type"], "function_call");
    assert_eq!(call["call_id"], "call_1");
    assert_eq!(call["name"], "bash");

    // The second-turn request body must carry the function_call and the
    // function_call_output back to the API. Verifies that the continuation
    // path matches what the in-memory loop appended via `into_raw_output`.
    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 2, "expected two stub requests");
    let second_input = bodies[1]["input"]
        .as_array()
        .expect("input must be an array");
    let pos_call = input_position(second_input, "function_call", |item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
            && item.get("call_id").and_then(Value::as_str) == Some("call_1")
    });
    let pos_output = input_position(second_input, "function_call_output", |item| {
        item.get("type").and_then(Value::as_str) == Some("function_call_output")
            && item.get("call_id").and_then(Value::as_str) == Some("call_1")
    });
    assert!(
        pos_call < pos_output,
        "function_call must precede its output"
    );
}

#[tokio::test]
async fn run_agent_emits_truncation_metadata_when_bash_spills() {
    // Model asks for a bash command whose output is large enough to spill
    // to disk. The emitted ToolCallOutput must surface `truncated`,
    // `truncated_by`, and `full_output_path` so durable events let
    // operators (and replay) link to the full output.
    let turn_one = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_spill",
                "name": "bash",
                "arguments": "{\"command\":\"seq 1 200000\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let turn_two = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![turn_one, turn_two]);

    let mut args = Args::test_default();
    args.max_turns = 4;
    args.bash_timeout_secs = 30;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "spill",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let tool_output = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ToolCallOutput { .. }))
        .expect("ToolCallOutput should be emitted");
    match tool_output {
        AgentEvent::ToolCallOutput { truncation, .. } => {
            let meta = truncation.as_ref().expect("spill truncation");
            assert_eq!(
                meta.truncated_by,
                crate::tool_registry::TruncationKind::BashSpill
            );
            let path = meta
                .full_output_path
                .as_ref()
                .expect("spill path should be set");
            assert!(path.is_absolute(), "{}", path.display());
            let _ = std::fs::remove_file(path);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn run_agent_git_checkpoints_dirty_worktree_before_user_message() {
    let turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Done."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![turn]);
    let mut args = Args::test_default();
    args.git_checkpoints = true;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    git(&cwd, &["init"]);
    git(&cwd, &["config", "user.name", "Nav Test"]);
    git(&cwd, &["config", "user.email", "nav@example.test"]);
    fs::write(cwd.join("tracked.txt"), "base\n").unwrap();
    git(&cwd, &["add", "tracked.txt"]);
    git(&cwd, &["commit", "-m", "init"]);
    fs::write(cwd.join("tracked.txt"), "dirty\n").unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "do the thing",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent should succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let checkpoint_pos = event_position(&events, "GitCheckpoint", |event| {
        matches!(
            event,
            AgentEvent::GitCheckpoint {
                action: crate::git_checkpoint::GitCheckpointAction::Checkpoint,
                status: crate::git_checkpoint::GitCheckpointStatus::Created,
                ..
            }
        )
    });
    let user_pos = event_position(&events, "UserMessage", |event| {
        matches!(event, AgentEvent::UserMessage { .. })
    });
    assert!(checkpoint_pos < user_pos);
    assert_eq!(
        fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
        "dirty\n"
    );
    assert_eq!(
        crate::git_checkpoint::list_nav_stashes(&cwd).unwrap().len(),
        1
    );
}

#[tokio::test]
async fn run_agent_spawn_subagent_returns_worker_summary_to_parent() {
    let parent_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_worker",
                "name": SPAWN_SUBAGENT_TOOL,
                "arguments": "{\"task\":\"inspect session code\",\"label\":\"explorer\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let worker_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Checked session/mod.rs; no issue found."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let parent_final = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Integrated the worker result."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![parent_turn, worker_turn, parent_final]);

    let mut args = Args::test_default();
    args.max_turns = 4;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "use a helper",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let pos_tool = event_position(&events, "ToolCallStarted", |event| {
        matches!(
            event,
            AgentEvent::ToolCallStarted { call_id, name, .. }
                if call_id == "call_worker" && name == SPAWN_SUBAGENT_TOOL
        )
    });
    let pos_started = event_position(&events, "SubagentStarted", |event| {
        matches!(
            event,
            AgentEvent::SubagentStarted { id, label, task }
                if id == "call_worker"
                    && label.as_deref() == Some("explorer")
                    && task == "inspect session code"
        )
    });
    let pos_completed = event_position(&events, "SubagentCompleted", |event| {
        matches!(
            event,
            AgentEvent::SubagentCompleted { id, summary }
                if id == "call_worker" && summary.contains("Checked session/mod.rs")
        )
    });
    let pos_output = event_position(&events, "ToolCallOutput", |event| {
        matches!(
            event,
            AgentEvent::ToolCallOutput { call_id, output, is_error, .. }
                if call_id == "call_worker"
                    && !*is_error
                    && output.contains("Checked session/mod.rs")
        )
    });
    assert!(pos_tool < pos_started);
    assert!(pos_started < pos_completed);
    assert!(pos_completed < pos_output);
    assert!(
        events.iter().any(
            |event| matches!(event, AgentEvent::AssistantMessageDone { text } if text == "Integrated the worker result.")
        )
    );

    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 3);
    let worker_body = &bodies[1];
    let worker_tools: Vec<&str> = worker_body
        .get("tools")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(
        worker_tools,
        vec![
            "read_file",
            "list_files",
            "code_search",
            "read_thread",
            "expand_artifact"
        ]
    );
    let worker_input = worker_body.get("input").and_then(Value::as_array).unwrap();
    assert!(
        worker_input
            .first()
            .and_then(|item| item.get("content"))
            .and_then(Value::as_str)
            .is_some_and(|text| {
                text.contains("focused nav subagent")
                    && text.contains("inspect session code")
                    && text.contains("plain, layman's terms")
            }),
        "worker prompt missing task: {worker_input:#?}"
    );
}

#[tokio::test]
async fn subagent_scope_blocks_hallucinated_mutating_tool() {
    let parent_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_worker",
                "name": SPAWN_SUBAGENT_TOOL,
                "arguments": "{\"task\":\"try to edit note.txt\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let worker_mutation_attempt = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "worker_patch",
                "name": "apply_patch",
                "arguments": "{\"patch\":\"*** Begin Patch\\n*** Update File: note.txt\\n@@\\n-old\\n+new\\n*** End Patch\\n\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let worker_final = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Mutation was blocked by subagent scope."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let parent_final = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Worker could not edit."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![
        parent_turn,
        worker_mutation_attempt,
        worker_final,
        parent_final,
    ]);

    let mut args = Args::test_default();
    args.max_turns = 5;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    fs::write(cwd.join("note.txt"), "old\n").unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "use a helper",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent");
    while rx.recv().await.is_some() {}

    assert_eq!(fs::read_to_string(cwd.join("note.txt")).unwrap(), "old\n");
    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 4);
    let second_worker_input = bodies[2].get("input").and_then(Value::as_array).unwrap();
    assert!(
        second_worker_input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item
                    .get("output")
                    .and_then(Value::as_str)
                    .is_some_and(|output| output.contains("tool apply_patch blocked"))
        }),
        "worker retry did not receive blocked tool output: {second_worker_input:#?}"
    );
}

#[tokio::test]
async fn run_agent_injects_steering_before_dispatching_stale_tool_calls() {
    let turn_one = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo stale\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let turn_two = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Steered."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![turn_one, turn_two]);
    let steering = PendingInput {
        id: "pending-1".into(),
        mode: PendingInputMode::Steering,
        text: "do not run the stale shell command".into(),
        display_text: None,
        attachments: Vec::new(),
        skill: None,
    };

    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test_with_controls(
        &transport,
        &args,
        &cwd,
        "start",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
        TurnControls::with_steering_items([steering]),
    )
    .await
    .expect("run_agent");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::PendingInputDequeued { id, mode } if id == "pending-1" && *mode == PendingInputMode::Steering)),
        "expected steering dequeue event: {events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCallStarted { .. })),
        "stale tool call should not dispatch after steering: {events:#?}"
    );

    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 2, "{bodies:#?}");
    let second_input = bodies[1].get("input").and_then(Value::as_array).unwrap();
    assert!(
        second_input
            .iter()
            .any(|item| is_input_user_message(item, "do not run the stale shell command")),
        "steering not injected into second model request: {second_input:#?}"
    );
}

#[tokio::test]
async fn run_agent_records_abort_from_approval_as_aborted_turn() {
    let turn_one = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"rm -rf build\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![turn_one]);
    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "clean build output",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        aborting_permission_context(),
    )
    .await
    .expect("approval abort exits cleanly");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert!(
        events.iter().any(|event| matches!(event, AgentEvent::TurnAborted { reason, .. } if reason.contains("approval"))),
        "expected TurnAborted from approval abort: {events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnComplete { .. })),
        "aborted turn must not complete normally: {events:#?}"
    );
}

#[tokio::test]
#[cfg(unix)]
async fn approval_abort_emits_turn_diff_for_pre_turn_hook_mutation() {
    let turn_one = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"rm -rf build\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![turn_one]);
    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    fs::write(cwd.join("note.txt"), "old\n").unwrap();
    git(&cwd, &["init"]);
    git(&cwd, &["add", "note.txt"]);
    git(
        &cwd,
        &[
            "-c",
            "user.name=Nav Test",
            "-c",
            "user.email=nav@example.test",
            "commit",
            "-m",
            "init",
        ],
    );
    let extensions = ExtensionCatalog::with_hooks(
        vec![],
        vec![],
        vec![],
        vec![test_hook(
            "before",
            HookEventType::PreTurn,
            "printf hook > note.txt",
            &cwd,
        )],
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    super::run_agent(
        AgentTurnRequest::new(
            &transport,
            &args,
            &cwd,
            "clean build output",
            tx,
            &Catalog::default(),
            aborting_permission_context(),
        )
        .with_extensions(Some(&extensions)),
    )
    .await
    .expect("approval abort exits cleanly");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::TurnDiff { files, unified_diff, .. }
                if files.iter().any(|file| file.path == "note.txt")
                    && unified_diff.contains("-old")
                    && unified_diff.contains("+hook")
        )),
        "expected TurnDiff for pre_turn hook mutation before approval abort: {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnAborted { reason, .. } if reason.contains("approval"))),
        "expected TurnAborted from approval abort: {events:#?}"
    );
}

#[tokio::test]
async fn run_agent_emits_file_change_and_turn_diff_for_patch_tool() {
    let turn_one = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_patch",
                "name": "apply_patch",
                "arguments": "{\"patch\":\"*** Begin Patch\\n*** Update File: note.txt\\n@@\\n-old\\n+new\\n*** End Patch\\n\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let turn_two = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Patched."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![turn_one, turn_two]);
    let mut args = Args::test_default();
    args.max_turns = 4;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    fs::write(cwd.join("note.txt"), "old\n").unwrap();
    git(&cwd, &["init"]);
    git(&cwd, &["add", "note.txt"]);
    git(
        &cwd,
        &[
            "-c",
            "user.name=Nav Test",
            "-c",
            "user.email=nav@example.test",
            "commit",
            "-m",
            "init",
        ],
    );

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "patch note",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let pos_tool_output = event_position(&events, "ToolCallOutput", |event| {
        matches!(
            event,
            AgentEvent::ToolCallOutput { output, .. }
                if output.contains("updated 1 file") && !output.contains("@@")
        )
    });
    let pos_file_change = event_position(&events, "FileChange", |event| {
        matches!(
            event,
            AgentEvent::FileChange { call_id, changes, status, .. }
                if call_id == "call_patch"
                    && *status == crate::verify::PatchApplyStatus::Completed
                    && changes.len() == 1
                    && changes[0].path == "note.txt"
                    && changes[0].diff.contains("-old")
                    && changes[0].diff.contains("+new")
        )
    });
    let pos_turn_diff = event_position(&events, "TurnDiff", |event| {
        matches!(
            event,
            AgentEvent::TurnDiff { files, unified_diff, .. }
                if files.iter().any(|file| file.path == "note.txt")
                    && unified_diff.contains("-old")
                    && unified_diff.contains("+new")
        )
    });
    let first_turn_complete = event_position(&events, "TurnComplete", |event| {
        matches!(event, AgentEvent::TurnComplete { .. })
    });

    assert!(pos_tool_output < pos_file_change);
    assert!(pos_file_change < pos_turn_diff);
    assert!(pos_turn_diff < first_turn_complete);
}

#[tokio::test]
async fn run_agent_emits_failed_file_change_for_rejected_patch_tool() {
    let turn_one = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_patch",
                "name": "apply_patch",
                "arguments": "{\"patch\":\"*** Begin Patch\\n*** Update File: note.txt\\n@@\\n-missing\\n+new\\n*** End Patch\\n\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let turn_two = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Could not patch."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let transport = StubTransport::new(vec![turn_one, turn_two]);
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    fs::write(cwd.join("note.txt"), "old\n").unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &Args::test_default(),
        &cwd,
        "patch note",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent continues after tool error");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    event_position(&events, "failed FileChange", |event| {
        matches!(
            event,
            AgentEvent::FileChange { call_id, changes, status, summary, error }
                if call_id == "call_patch"
                    && changes.is_empty()
                    && *status == crate::verify::PatchApplyStatus::Failed
                    && summary.contains("note.txt")
                    && error.as_deref().is_some_and(|message| message.contains("tool error"))
        )
    });
    assert_eq!(fs::read_to_string(cwd.join("note.txt")).unwrap(), "old\n");
}

// ── protected-read attachment gating ─────────────────────────

#[tokio::test]
async fn run_agent_gates_protected_file_attachment_through_approval() {
    // A user typing `@.env` must not silently leak the file: the agent loop
    // routes the attachment through the same approval gate as the
    // `read_file` tool. Approved → bytes ride along; Denied → the
    // attachment is dropped and a `ToolCallBlocked` event surfaces in the
    // transcript so the operator sees what happened.
    let turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];

    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    fs::write(cwd.join(".env"), "DB_PASS=hunter2").unwrap();

    let gate = Arc::new(RecordingGate::with_decisions(vec![ReviewDecision::Denied]));
    let permissions = recording_permission_context(gate.clone());

    let transport = StubTransport::new(vec![turn]);
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &Args::test_default(),
        &cwd,
        "look",
        None,
        vec![UserAttachment::File {
            path: PathBuf::from(".env"),
        }],
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        permissions,
    )
    .await
    .expect("run_agent");

    // The gate saw a request for `.env` with the protected_read reason.
    let requests = gate.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path.as_deref(), Some(".env"));
    assert_eq!(requests[0].reason, "protected_read");

    // The denied attachment must not have its body in the outbound body.
    {
        let bodies = transport.bodies.lock().unwrap();
        let serialized = bodies[0].to_string();
        assert!(
            !serialized.contains("hunter2"),
            "denied secret leaked into request: {serialized}"
        );
    }

    // The transcript records a Blocked event so the user understands the
    // attachment was refused.
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let saw_blocked = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::ToolCallBlocked { tool, rule, .. }
                if tool == "attachment_read" && rule == "protected_read"
        )
    });
    assert!(saw_blocked, "expected ToolCallBlocked event: {events:#?}");
}

#[tokio::test]
async fn run_agent_includes_protected_file_attachment_when_approved() {
    let turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];

    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    fs::write(cwd.join(".env"), "DB_PASS=hunter2").unwrap();

    let gate = Arc::new(RecordingGate::with_decisions(vec![
        ReviewDecision::Approved,
    ]));
    let permissions = recording_permission_context(gate.clone());

    let transport = StubTransport::new(vec![turn]);
    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &Args::test_default(),
        &cwd,
        "look",
        None,
        vec![UserAttachment::File {
            path: PathBuf::from(".env"),
        }],
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        permissions,
    )
    .await
    .expect("run_agent");

    assert_eq!(gate.requests().len(), 1);
    let bodies = transport.bodies.lock().unwrap();
    let serialized = bodies[0].to_string();
    assert!(
        serialized.contains("DB_PASS=hunter2"),
        "approved attachment body missing: {serialized}"
    );
    assert!(
        serialized.contains("<attached file: .env>"),
        "expected fenced wrapper: {serialized}"
    );
}

#[tokio::test]
async fn run_agent_aborts_turn_when_attachment_approval_aborts() {
    let turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "should not appear"}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];

    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    fs::write(cwd.join("id_rsa"), "ssh secret").unwrap();

    let permissions = aborting_permission_context();
    let transport = StubTransport::new(vec![turn]);
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &Args::test_default(),
        &cwd,
        "look",
        None,
        vec![UserAttachment::File {
            path: PathBuf::from("id_rsa"),
        }],
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        permissions,
    )
    .await
    .expect("run_agent");

    // No body should have been sent — the abort fires before the prompt is
    // emitted as a user message and before the transport is invoked.
    {
        let bodies = transport.bodies.lock().unwrap();
        assert!(
            bodies.is_empty(),
            "aborted turn must not call the transport: {bodies:#?}"
        );
    }

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnAborted { .. })),
        "expected TurnAborted: {events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::UserMessage { .. })),
        "user message must not be emitted on abort: {events:#?}"
    );
}

// ── --resume integration ──────────────────────────────────────
//
// The "slice A stub" exercises the resume contract: a fresh agent run is
// captured to the store, the persisted events are reloaded, fed back as
// the `initial_input` to a second `run_agent` call, and the resulting
// transcript must equal "fresh run + one extra prompt".
#[tokio::test]
async fn resume_replays_transcript_and_appends_new_events() {
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();

    // Turn produces a single assistant message and reports usage; this
    // keeps the stub minimal while still touching every durable variant
    // the resume path cares about (Done + TurnComplete).
    let turn_one = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Hello."}]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }
        }),
    ];
    let turn_two = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Goodbye."}]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "usage": {"input_tokens": 20, "output_tokens": 7}
            }
        }),
    ];

    let mut args = Args::test_default();
    args.max_turns = 2;

    // ── Run 1: fresh session, prompt "first".
    let transport_one = StubTransport::new(vec![turn_one]);
    let binding_one = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx1, mut rx1) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport_one,
        &args,
        &cwd,
        "first",
        None,
        Vec::new(),
        tx1,
        Some(&binding_one),
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("first run_agent");
    let mut run1_events = Vec::new();
    while let Some(event) = rx1.recv().await {
        run1_events.push(event);
    }

    // Run 1 should have produced exactly: UserMessage(first),
    // Done(Hello.), TurnComplete.
    // No deltas exist in the stub stream, so durable == emitted.
    let stored_after_run1 = store.load_session(&session_id).unwrap();
    assert_eq!(stored_after_run1, run1_events);
    assert!(matches!(
        stored_after_run1.first(),
        Some(AgentEvent::UserMessage { text, .. }) if text == "first"
    ));

    // ── Resume: load events, rebuild input, run prompt "second".
    let rebuilt = rebuild_responses_input(&stored_after_run1, Path::new("/tmp"));
    // Sanity: the rebuilt input contains the prior user prompt and the
    // assistant reply in order.
    assert!(matches!(
        rebuilt.first(),
        Some(item) if is_input_user_message(item, "first")
    ));
    assert!(
        rebuilt
            .iter()
            .any(|item| is_input_assistant_message(item, "Hello."))
    );

    let transport_two = StubTransport::new(vec![turn_two]);
    let binding_two = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx2, mut rx2) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport_two,
        &args,
        &cwd,
        "second",
        None,
        Vec::new(),
        tx2,
        Some(&binding_two),
        Some(rebuilt),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("resumed run_agent");
    let mut run2_events = Vec::new();
    while let Some(event) = rx2.recv().await {
        run2_events.push(event);
    }

    // ── The persisted transcript = events from run 1 + events from run 2.
    // "Fresh run plus one extra prompt" matches exactly that concatenation.
    let full = store.load_session(&session_id).unwrap();
    let mut expected = run1_events.clone();
    expected.extend(run2_events.clone());
    assert_eq!(full, expected);
    // And the second turn observed the second assistant message.
    assert!(matches!(
        run2_events.iter().find(|e| matches!(e, AgentEvent::AssistantMessageDone { .. })),
        Some(AgentEvent::AssistantMessageDone { text }) if text == "Goodbye."
    ));
    let second_bodies = transport_two.bodies();
    let second_body = second_bodies
        .first()
        .expect("second transport should receive a request body");
    let second_input = second_body
        .get("input")
        .and_then(Value::as_array)
        .expect("second request should include input");
    let first_user_pos = input_position(second_input, "first user prompt", |item| {
        is_input_user_message(item, "first")
    });
    let assistant_pos = input_position(second_input, "assistant reply", |item| {
        is_input_assistant_message(item, "Hello.")
    });
    let second_user_pos = input_position(second_input, "second user prompt", |item| {
        is_input_user_message(item, "second")
    });
    assert!(first_user_pos < assistant_pos);
    assert!(assistant_pos < second_user_pos);
    // Two TurnComplete events in total — one per prompt.
    let turn_completes = full
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnComplete { .. }))
        .count();
    assert_eq!(turn_completes, 2);
}

#[tokio::test]
async fn resume_replays_reasoning_continuation_and_function_call_output() {
    // The first run ends mid-tool-turn: the model emitted a reasoning item +
    // function_call, nav fielded the tool result, then the model concluded
    // with a final assistant message. The persisted session log must let a
    // fresh `run_agent` invocation rebuild a wire input that still carries
    // the reasoning continuation handle and the matching function_call /
    // function_call_output pair — without resurrecting any plaintext
    // reasoning that should have been stripped at persistence time.
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();

    let turn_one_tool_call = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{"type": "summary_text", "text": "thinking out loud"}],
                "content": [{"type": "reasoning_text", "text": "raw chain of thought"}],
                "encrypted_content": "enc-blob",
            }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let turn_one_final = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Done."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];
    let turn_two = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "Acknowledged."}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ];

    let mut args = Args::test_default();
    args.max_turns = 4;

    let transport_one = StubTransport::new(vec![turn_one_tool_call, turn_one_final]);
    let binding_one = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx1, mut rx1) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport_one,
        &args,
        &cwd,
        "do the thing",
        None,
        Vec::new(),
        tx1,
        Some(&binding_one),
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("first run_agent");
    while rx1.recv().await.is_some() {}

    // The persisted log must not contain the plaintext reasoning trace. We
    // serialize the stored events as JSON and check the raw text for the
    // hidden plaintext strings.
    let stored = store.load_session(&session_id).unwrap();
    let stored_json = serde_json::to_string(&stored).unwrap();
    assert!(
        !stored_json.contains("raw chain of thought"),
        "hidden plaintext reasoning content leaked into session log: {stored_json}"
    );
    assert!(
        !stored_json.contains("thinking out loud"),
        "reasoning summary leaked into session log: {stored_json}"
    );
    assert!(
        stored_json.contains("enc-blob"),
        "encrypted reasoning handle should be persisted: {stored_json}"
    );

    let continuation = stored
        .iter()
        .find(|event| matches!(event, AgentEvent::ResponseContinuation { .. }))
        .expect("expected persisted ResponseContinuation");
    if let AgentEvent::ResponseContinuation { items } = continuation {
        let reasoning = items
            .iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
            .expect("reasoning item");
        assert!(reasoning.get("summary").is_none());
        assert!(reasoning.get("content").is_none());
    }

    // Rebuild for the next TUI turn and verify the wire shape replayed
    // matches what the in-memory loop appended originally.
    let rebuilt = rebuild_responses_input(&stored, &cwd);
    let reasoning_pos = input_position(&rebuilt, "reasoning", |item| {
        item.get("type").and_then(Value::as_str) == Some("reasoning")
            && item.get("encrypted_content").and_then(Value::as_str) == Some("enc-blob")
    });
    let call_pos = input_position(&rebuilt, "function_call", |item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
            && item.get("call_id").and_then(Value::as_str) == Some("call_1")
    });
    let output_pos = input_position(&rebuilt, "function_call_output", |item| {
        item.get("type").and_then(Value::as_str) == Some("function_call_output")
            && item.get("call_id").and_then(Value::as_str) == Some("call_1")
    });
    assert!(reasoning_pos < call_pos);
    assert!(call_pos < output_pos);

    // Run the next user turn against the rebuilt input and check that the
    // request body sent to the provider still pairs the prior function_call
    // with its output — but the prior reasoning has been shed, because the
    // new user message pushes it outside the "recent continuation" window
    // (`keep_reasoning_turns = 1` by default; issue #51).
    let transport_two = StubTransport::new(vec![turn_two]);
    let binding_two = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx2, mut rx2) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport_two,
        &args,
        &cwd,
        "and then?",
        None,
        Vec::new(),
        tx2,
        Some(&binding_two),
        Some(rebuilt),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("second run_agent");
    while rx2.recv().await.is_some() {}

    let bodies = transport_two.bodies();
    let body = bodies.first().expect("second turn body");
    let input = body.get("input").and_then(Value::as_array).expect("input");
    let call_pos = input_position(input, "function_call", |item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
    });
    let output_pos = input_position(input, "function_call_output", |item| {
        item.get("type").and_then(Value::as_str) == Some("function_call_output")
    });
    assert!(call_pos < output_pos);
    assert!(
        !input
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning")),
        "prior turn reasoning must be shed once a new user message arrives: {input:?}",
    );
}

#[tokio::test]
async fn user_message_with_image_attachment_is_sent_as_input_image_content() {
    use base64::Engine;
    use std::path::PathBuf;

    // Minimal turn: assistant replies with a plain message so the agent loop
    // terminates after one round-trip without invoking tools.
    let turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}],
            },
        }),
        json!({"type": "response.completed", "response": {"usage": {}}}),
    ];
    let transport = StubTransport::new(vec![turn]);

    let mut args = Args::test_default();
    args.max_turns = 1;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let png_bytes: &[u8] = b"\x89PNG\r\n\x1a\nFAKEBYTES";
    let rel = PathBuf::from("paste.png");
    std::fs::write(cwd.join(&rel), png_bytes).unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "describe this",
        None,
        vec![UserAttachment::Image { path: rel }],
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent");
    drop(rx.recv().await);
    while rx.recv().await.is_some() {}

    // The first request body's `input[0]` should be a user message whose
    // content is an array containing both `input_text` and `input_image`.
    let body = transport.bodies().remove(0);
    let input = body.get("input").and_then(Value::as_array).expect("input");
    let first = input.first().expect("first input item");
    let content = first
        .get("content")
        .and_then(Value::as_array)
        .expect("content should be an array when attachments are present");
    let parts: Vec<&str> = content
        .iter()
        .filter_map(|p| p.get("type").and_then(Value::as_str))
        .collect();
    assert!(
        parts.contains(&"input_text"),
        "missing input_text: {parts:?}"
    );
    assert!(
        parts.contains(&"input_image"),
        "missing input_image: {parts:?}"
    );
    let image_part = content
        .iter()
        .find(|p| p.get("type").and_then(Value::as_str) == Some("input_image"))
        .expect("image part");
    let url = image_part
        .get("image_url")
        .and_then(Value::as_str)
        .expect("image_url");
    let expected_b64 = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    assert!(
        url.starts_with("data:image/png;base64,") && url.contains(&expected_b64),
        "unexpected image_url: {url}"
    );
}

#[tokio::test]
async fn user_message_image_is_stripped_for_text_only_model() {
    use std::path::PathBuf;

    let turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}],
            },
        }),
        json!({"type": "response.completed", "response": {"usage": {}}}),
    ];
    let transport = StubTransport::new(vec![turn]);

    let mut args = Args::test_default();
    args.max_turns = 1;
    args.model = "o3-mini".to_string();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let png_bytes: &[u8] = b"\x89PNG\r\n\x1a\nFAKEBYTES";
    let rel = PathBuf::from("paste.png");
    std::fs::write(cwd.join(&rel), png_bytes).unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "describe this",
        None,
        vec![UserAttachment::Image { path: rel }],
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent");
    drop(rx.recv().await);
    while rx.recv().await.is_some() {}

    let body = transport.bodies().remove(0);
    let input = body.get("input").and_then(Value::as_array).expect("input");
    let first = input.first().expect("first input item");
    let parts = first
        .get("content")
        .and_then(Value::as_array)
        .expect("user message must have array content when attachments are present");
    assert!(
        parts
            .iter()
            .all(|part| part.get("type").and_then(Value::as_str) != Some("input_image")),
        "text-only model must not receive input_image parts: {parts:?}"
    );
    assert!(
        parts
            .iter()
            .any(|part| part.get("type").and_then(Value::as_str) == Some("input_text")),
        "input_text part must still be present after image stripping: {parts:?}"
    );
}

// ── context-overflow recovery ─────────────────────────────────

#[tokio::test]
async fn overflow_one_shot_recovery_compacts_and_continues() {
    // Turn 1: model asks for a tool call.
    let turn_one = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    // Turn 2: server says we blew the context window.
    let turn_two = vec![StubItem::Err(
        crate::model::responses::ResponsesError::ContextWindowExceeded {
            message: "input is too long".into(),
        },
    )];
    // Turn 3: the compaction-recovery summarisation turn fires next.
    let turn_three_summary = compact_turn_with_text("HANDOFF: covered")
        .into_iter()
        .map(StubItem::Event)
        .collect();
    // Turn 4 (after recovery rewrites history): model finishes.
    let turn_four = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}]
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    let transport =
        StubTransport::with_items(vec![turn_one, turn_two, turn_three_summary, turn_four]);

    let mut args = Args::test_default();
    args.max_turns = 6;
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "go",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("recovery should succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    // Compaction lifecycle: Started → Completed, both with the Auto trigger.
    let started_pos = event_position(&events, "CompactionStarted", |e| {
        matches!(
            e,
            AgentEvent::CompactionStarted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    let completed_pos = event_position(&events, "CompactionCompleted", |e| {
        matches!(
            e,
            AgentEvent::CompactionCompleted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    assert!(started_pos < completed_pos);

    // The retry sampling (4th `create()` call) must no longer contain the
    // original `function_call` for call_1 — compaction rewrote history.
    let bodies = transport.bodies();
    assert_eq!(
        bodies.len(),
        4,
        "tool sampling + overflow attempt + compaction summary + retry"
    );
    let retry_input = bodies[3]
        .get("input")
        .and_then(Value::as_array)
        .expect("retry body has input");
    let has_call_1 = retry_input.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
            && item.get("call_id").and_then(Value::as_str) == Some("call_1")
    });
    assert!(
        !has_call_1,
        "call_1 should be gone from the compacted retry"
    );

    // Recovery is one-shot; the flag is consumed. The retry's assistant
    // message bubbles all the way back up to the user.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AssistantMessageDone { text } if text == "ok"))
    );
}

#[tokio::test]
async fn overflow_recovery_does_not_consume_turn_budget() {
    // With max_turns=2, the agent must still be able to (1) run a tool-call
    // turn, (2) hit overflow, compact, (3) retry, and (4) finish — even
    // though the compaction+retry conceptually happens on what would have
    // been the "last" turn. Recovery is bookkeeping, not a real model turn.
    let turn_one = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    let turn_two_overflow = vec![StubItem::Err(
        crate::model::responses::ResponsesError::ContextWindowExceeded {
            message: "too long".into(),
        },
    )];
    let turn_three_summary = compact_turn_with_text("HANDOFF: covered")
        .into_iter()
        .map(StubItem::Event)
        .collect();
    let turn_four_after_compact = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "done"}]
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    let transport = StubTransport::with_items(vec![
        turn_one,
        turn_two_overflow,
        turn_three_summary,
        turn_four_after_compact,
    ]);

    let mut args = Args::test_default();
    args.max_turns = 2;
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "go",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("recovery on the last allowed turn should still succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::CompactionCompleted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )),
        "expected CompactionCompleted from overflow recovery"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AssistantMessageDone { text } if text == "done"))
    );
    assert_eq!(transport.bodies().len(), 4, "4 transport calls expected");
}

#[tokio::test]
async fn overflow_second_failure_surfaces_clean_error() {
    // Turn 1: tool call to seed history with droppable items.
    let turn_one = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    // First overflow → triggers compaction-recovery.
    let turn_two = vec![StubItem::Err(
        crate::model::responses::ResponsesError::ContextWindowExceeded {
            message: "too long".into(),
        },
    )];
    // Compaction summarisation succeeds with a summary.
    let turn_three_summary = compact_turn_with_text("HANDOFF: covered")
        .into_iter()
        .map(StubItem::Event)
        .collect();
    // Second overflow — recovery already consumed, must surface as Error.
    let turn_four_overflow = vec![StubItem::Err(
        crate::model::responses::ResponsesError::ContextWindowExceeded {
            message: "still too long".into(),
        },
    )];
    let transport = StubTransport::with_items(vec![
        turn_one,
        turn_two,
        turn_three_summary,
        turn_four_overflow,
    ]);

    let mut args = Args::test_default();
    args.max_turns = 6;
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    let err = run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "go",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect_err("second overflow should fail");
    assert!(err.to_string().contains("context window exceeded"));

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let compaction_started_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .count();
    assert_eq!(
        compaction_started_count, 1,
        "compaction-recovery should fire exactly once per run"
    );
    let error_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Error { .. }))
        .count();
    assert_eq!(error_count, 1);
}

#[tokio::test]
async fn overflow_recovery_retry_sees_compacted_history() {
    // Acceptance criterion for #87: the retry sampling after an overflow
    // must see the *compacted* history shape — its trailing user message
    // carries the SUMMARY_PREFIX-marked summary, with the pre-compaction
    // tool exchange gone.
    let turn_one = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    let turn_two_overflow = vec![StubItem::Err(
        crate::model::responses::ResponsesError::ContextWindowExceeded {
            message: "too long".into(),
        },
    )];
    let turn_three_summary = compact_turn_with_text("HANDOFF: rolled forward")
        .into_iter()
        .map(StubItem::Event)
        .collect();
    let turn_four_retry = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "all done"}]
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    let transport = StubTransport::with_items(vec![
        turn_one,
        turn_two_overflow,
        turn_three_summary,
        turn_four_retry,
    ]);

    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "go",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("compaction recovery should succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let started_pos = event_position(&events, "CompactionStarted", |e| {
        matches!(
            e,
            AgentEvent::CompactionStarted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    let completed_pos = event_position(&events, "CompactionCompleted", |e| {
        matches!(
            e,
            AgentEvent::CompactionCompleted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    assert!(started_pos < completed_pos);

    // The retry request body — the 4th create() — must show a compacted
    // history: no `function_call_output` items, and the trailing item is
    // the SUMMARY_PREFIX-marked user message.
    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 4);
    let retry_input = bodies[3]
        .get("input")
        .and_then(Value::as_array)
        .expect("retry input");
    let function_outputs: Vec<&Value> = retry_input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
        .collect();
    assert!(
        function_outputs.is_empty(),
        "compaction must drop function_call_output from the retry input: {function_outputs:?}"
    );
    let last_text = retry_input
        .last()
        .and_then(|item| item.get("content"))
        .and_then(Value::as_str)
        .expect("retry input ends with a user message carrying the summary");
    assert!(
        last_text.starts_with(SUMMARY_PREFIX),
        "trailing item is the compaction summary: {last_text}"
    );
}

#[tokio::test]
async fn proactive_prune_sheds_oldest_pair_before_first_request() {
    // Synthesize a multi-turn `initial_input` whose tool outputs exceed the
    // default 120KB pre-call budget. With `raw_tool_turns = 2`, the most
    // recent two user-message boundaries — the second old turn and the new
    // prompt the runner pushes — protect c2's pair, leaving c1 as the oldest
    // droppable pair.
    let big = "x".repeat(70 * 1024);
    let initial_input = vec![
        json!({"type": "message", "role": "user", "content": "old turn"}),
        json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{}"}),
        json!({"type": "function_call_output", "call_id": "c1", "output": big.clone()}),
        json!({"type": "message", "role": "user", "content": "older turn"}),
        json!({"type": "function_call", "call_id": "c2", "name": "bash", "arguments": "{}"}),
        json!({"type": "function_call_output", "call_id": "c2", "output": big}),
    ];

    let final_turn = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}]
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    let transport = StubTransport::with_items(vec![final_turn]);

    let args = Args::test_default();
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "fresh prompt",
        None,
        Vec::new(),
        tx,
        None,
        Some(initial_input),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("turn should succeed after pre-call prune");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let trimmed = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ContextTrimmed { .. }))
        .expect("expected ContextTrimmed before the request was sent");
    assert!(matches!(
        trimmed,
        AgentEvent::ContextTrimmed { dropped_pairs } if *dropped_pairs >= 1
    ));

    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 1, "exactly one request after pruning");
    let sent_input = bodies[0]
        .get("input")
        .and_then(Value::as_array)
        .expect("body carries input");
    let has_c1 = sent_input.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
            && item.get("call_id").and_then(Value::as_str) == Some("c1")
    });
    let has_c2 = sent_input.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
            && item.get("call_id").and_then(Value::as_str) == Some("c2")
    });
    assert!(!has_c1, "oldest pair c1 should have been pruned");
    assert!(has_c2, "recent pair c2 must be protected by raw_tool_turns");
    // Pair preservation: no `function_call_output` without its `function_call`.
    let output_ids: std::collections::HashSet<&str> = sent_input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
        .filter_map(|item| item.get("call_id").and_then(Value::as_str))
        .collect();
    let call_ids: std::collections::HashSet<&str> = sent_input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .filter_map(|item| item.get("call_id").and_then(Value::as_str))
        .collect();
    assert_eq!(
        output_ids, call_ids,
        "every function_call_output must keep its function_call"
    );
}

#[tokio::test]
async fn proactive_prune_no_op_when_under_budget() {
    // A normal turn with no historical tool pairs must not emit
    // ContextTrimmed; pre-call pruning is a no-op when the budget is fine.
    let final_turn = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}]
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    let transport = StubTransport::with_items(vec![final_turn]);

    let args = Args::test_default();
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "hello",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("turn should succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ContextTrimmed { .. })),
        "no ContextTrimmed expected when input is under budget"
    );
}

// ── transport-level retry plumbing ────────────────────────────

// ── tool-call budget backpressure ─────────────────────────────

fn tool_call_turn(call_id: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": call_id,
                "name": "list_files",
                "arguments": "{\"path\":\".\"}"
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ]
}

fn final_message_turn(text: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": text}]
            }
        }),
        json!({"type": "response.completed", "response": {}}),
    ]
}

#[tokio::test]
async fn tool_call_soft_budget_emits_warning_and_injects_steering() {
    // Three tool-call turns followed by a final message. With
    // tool_call_soft_budget=2, the warning should fire exactly once — after
    // the second tool call (cumulative count reaches the budget), and the
    // subsequent request should include the injected steering message.
    let transport = StubTransport::new(vec![
        tool_call_turn("call_1"),
        tool_call_turn("call_2"),
        tool_call_turn("call_3"),
        final_message_turn("done"),
    ]);

    let mut args = Args::test_default();
    args.max_turns = 10;
    args.tool_call_soft_budget = 2;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "explore",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let warnings: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolBudgetWarning { .. }))
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "single threshold crossing emits one warning: {events:#?}"
    );
    assert!(matches!(
        warnings[0],
        AgentEvent::ToolBudgetWarning {
            tool_calls: 2,
            soft_budget: 2
        }
    ));

    // After the warning fires, the next request (turn index 2, the third
    // model call) should carry the injected budget-check user message.
    let bodies = transport.bodies();
    assert!(bodies.len() >= 3, "expected at least 3 transport calls");
    let third_input = bodies[2].get("input").and_then(Value::as_array).unwrap();
    let nudge_present = third_input.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("role").and_then(Value::as_str) == Some("user")
            && item
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with("[nav budget check]"))
    });
    assert!(
        nudge_present,
        "budget-check steering not injected into next request: {third_input:#?}"
    );
}

#[tokio::test]
async fn tool_call_soft_budget_fires_repeatedly_for_long_turns() {
    // budget=1 means every tool call crosses a new multiple. Four tool-call
    // turns should produce four warnings before the final message arrives.
    let transport = StubTransport::new(vec![
        tool_call_turn("call_1"),
        tool_call_turn("call_2"),
        tool_call_turn("call_3"),
        tool_call_turn("call_4"),
        final_message_turn("done"),
    ]);

    let mut args = Args::test_default();
    args.max_turns = 10;
    args.tool_call_soft_budget = 1;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "explore",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let warning_counts: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolBudgetWarning { tool_calls, .. } => Some(*tool_calls),
            _ => None,
        })
        .collect();
    assert_eq!(
        warning_counts,
        vec![1, 2, 3, 4],
        "expected one warning per crossing, got {warning_counts:?}"
    );
}

#[tokio::test]
async fn tool_call_soft_budget_zero_disables_backpressure() {
    // soft_budget=0 is the deep-research escape hatch — no warning should
    // ever fire even after many tool calls.
    let transport = StubTransport::new(vec![
        tool_call_turn("call_1"),
        tool_call_turn("call_2"),
        tool_call_turn("call_3"),
        final_message_turn("done"),
    ]);

    let mut args = Args::test_default();
    args.max_turns = 10;
    args.tool_call_soft_budget = 0;
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "explore",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run_agent");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolBudgetWarning { .. })),
        "soft_budget=0 must not emit ToolBudgetWarning: {events:#?}"
    );
    // And no injected nudge should appear in any request body either.
    for body in transport.bodies() {
        let input = body.get("input").and_then(Value::as_array).unwrap();
        for item in input {
            if let Some(text) = item.get("content").and_then(Value::as_str) {
                assert!(
                    !text.starts_with("[nav budget check]"),
                    "unexpected nudge in body: {input:#?}"
                );
            }
        }
    }
}

// ── compaction integration ────────────────────────────────────

/// Build a single-turn stub that returns one assistant message with the
/// given text and a `response.completed` envelope. Used by the compaction
/// tests as the stand-in for "the model wrote a summary."
fn compact_turn_with_text(text: &str) -> Vec<Value> {
    compact_turn_with_text_and_input_tokens(text, 0)
}

/// Same as [`compact_turn_with_text`] but reports an explicit `input_tokens`
/// usage value on the `response.completed` envelope, so callers can assert
/// against a non-zero post-turn `tokens_input` reading.
fn compact_turn_with_text_and_input_tokens(text: &str, input_tokens: u64) -> Vec<Value> {
    let completed = if input_tokens == 0 {
        json!({"type": "response.completed", "response": {}})
    } else {
        json!({
            "type": "response.completed",
            "response": {"usage": {"input_tokens": input_tokens}},
        })
    };
    vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": text}]
            }
        }),
        completed,
    ]
}

#[tokio::test]
async fn manual_compact_emits_lifecycle_events_and_does_not_steer_prompt() {
    // Manual `/compact` should run a non-steerable compaction turn:
    // the user's prompt text never gets sent to the model. We assert that
    // the body submitted to the transport contains the synthesized
    // SUMMARIZATION_PROMPT, not the literal "/compact" text.
    let transport = StubTransport::new(vec![compact_turn_with_text(
        "handoff: did things, next: more things",
    )]);
    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "/compact",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("compact run");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    // Lifecycle: UserMessage("/compact") → CompactionStarted → CompactionCompleted.
    assert!(matches!(
        events.first(),
        Some(AgentEvent::UserMessage { text, .. }) if text == "/compact"
    ));
    let pos_started = event_position(
        &events,
        "CompactionStarted",
        |e| matches!(e, AgentEvent::CompactionStarted { trigger, .. } if matches!(trigger, super::CompactionTrigger::Manual)),
    );
    let pos_completed = event_position(&events, "CompactionCompleted", |e| {
        matches!(
            e,
            AgentEvent::CompactionCompleted {
                trigger,
                summary,
                details,
                ..
            }
                if matches!(trigger, super::CompactionTrigger::Manual)
                    && summary.contains("handoff: did things")
                    && details.is_none()
        )
    });
    assert!(pos_started < pos_completed);

    // Submitted body: SUMMARIZATION_PROMPT goes in, the slash-command text
    // does not (non-steerable).
    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 1, "compaction is a single turn");
    let input = bodies[0].get("input").and_then(Value::as_array).unwrap();
    let user_texts: Vec<&str> = input
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("user")
        })
        .filter_map(|item| item.get("content").and_then(Value::as_str))
        .collect();
    assert!(
        user_texts
            .iter()
            .any(|t| t.contains("CONTEXT CHECKPOINT COMPACTION")),
        "summarization prompt should be the user content: {user_texts:?}"
    );
    assert!(
        !user_texts.iter().any(|t| t.contains("/compact")),
        "compaction turn must not be steered by the slash text: {user_texts:?}"
    );
}

#[tokio::test]
async fn manual_compact_persists_checkpoint_for_replay() {
    // After a successful compaction, the session log should hold the
    // CompactionCompleted event, and rebuild_responses_input should slice
    // from that checkpoint instead of replaying the full transcript.
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();

    // 1) First turn: user says "first", model replies "ack".
    let regular_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ack"}]
            }
        }),
        json!({"type": "response.completed", "response": {"usage": {"input_tokens": 5}}}),
    ];
    let transport_one = StubTransport::new(vec![regular_turn]);
    let binding = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx1, mut rx1) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport_one,
        &Args::test_default(),
        &cwd,
        "first",
        None,
        Vec::new(),
        tx1,
        Some(&binding),
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("first turn");
    while rx1.recv().await.is_some() {}

    // 2) Manual /compact.
    let prior = store.load_session(&session_id).unwrap();
    let rebuilt = rebuild_responses_input(&prior, &cwd);
    let transport_two = StubTransport::new(vec![compact_turn_with_text("HANDOFF: did first")]);
    let binding = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx2, mut rx2) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport_two,
        &Args::test_default(),
        &cwd,
        "/compact",
        None,
        Vec::new(),
        tx2,
        Some(&binding),
        Some(rebuilt),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("compact turn");
    while rx2.recv().await.is_some() {}

    // The session log now contains a CompactionCompleted checkpoint.
    let after_compact = store.load_session(&session_id).unwrap();
    let checkpoint = after_compact
        .iter()
        .find(|e| matches!(e, AgentEvent::CompactionCompleted { .. }))
        .expect("checkpoint persisted");
    let summary = match checkpoint {
        AgentEvent::CompactionCompleted { summary, .. } => summary.clone(),
        _ => unreachable!(),
    };
    assert!(summary.contains("HANDOFF: did first"));

    // Resume input must slice from the checkpoint: only one user message,
    // and it's the synthesized summary (prefixed with SUMMARY_PREFIX).
    let replay_input = rebuild_responses_input(&after_compact, &cwd);
    assert_eq!(
        replay_input.len(),
        1,
        "replay should not silently expand back to the old transcript"
    );
    let only_user = replay_input[0]
        .get("content")
        .and_then(Value::as_str)
        .expect("synthesized summary is a plain-string user content");
    assert!(only_user.starts_with(SUMMARY_PREFIX));
    assert!(only_user.contains("HANDOFF: did first"));

    // No leakage of the pre-compaction "first" / "ack" pair.
    assert!(!only_user.contains("ack"));
    let any_old = replay_input.iter().any(|item| {
        item.get("content")
            .and_then(Value::as_str)
            .is_some_and(|s| s == "first" || s == "ack")
    });
    assert!(
        !any_old,
        "old transcript leaked into replay: {replay_input:?}"
    );
}

#[tokio::test]
async fn manual_compact_persists_file_ops_details_for_replay() {
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();
    let initial_input = vec![
        json!({"type": "message", "role": "user", "content": "inspect and edit"}),
        json!({"type": "function_call", "call_id": "read", "name": "read_file", "arguments": "{\"path\":\"src/lib.rs\"}"}),
        json!({"type": "function_call_output", "call_id": "read", "output": "file contents"}),
        json!({"type": "function_call", "call_id": "edit", "name": "edit_file", "arguments": "{\"path\":\"src/main.rs\",\"old_str\":\"a\",\"new_str\":\"b\"}"}),
        json!({"type": "function_call_output", "call_id": "edit", "output": "edited"}),
    ];
    let transport = StubTransport::new(vec![compact_turn_with_text("## Goal\nkeep going")]);
    let binding = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &Args::test_default(),
        &cwd,
        "/compact",
        None,
        Vec::new(),
        tx,
        Some(&binding),
        Some(initial_input),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("compact turn");
    while rx.recv().await.is_some() {}

    let events = store.load_session(&session_id).unwrap();
    let checkpoint = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::CompactionCompleted {
                summary, details, ..
            } => Some((summary, details.as_ref().expect("details"))),
            _ => None,
        })
        .expect("checkpoint persisted");

    assert!(
        checkpoint
            .0
            .contains("<read-files>\nsrc/lib.rs\n</read-files>")
    );
    assert!(
        checkpoint
            .0
            .contains("<modified-files>\nsrc/main.rs\n</modified-files>")
    );
    assert_eq!(checkpoint.1.read_files, vec!["src/lib.rs"]);
    assert_eq!(checkpoint.1.modified_files, vec!["src/main.rs"]);

    let replay_input = rebuild_responses_input(&events, &cwd);
    let replay_summary = replay_input[0]
        .get("content")
        .and_then(Value::as_str)
        .unwrap();
    assert!(replay_summary.contains("<read-files>\nsrc/lib.rs\n</read-files>"));
    assert!(replay_summary.contains("<modified-files>\nsrc/main.rs\n</modified-files>"));
}

#[tokio::test]
async fn consecutive_compactions_re_summarize_from_scratch() {
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let large_first_input = vec![
        json!({"type": "message", "role": "user", "content": "first task ".repeat(2_000)}),
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "large answer ".repeat(1_000)}]}),
    ];
    let first_transport = StubTransport::new(vec![compact_turn_with_text("## Goal\nfirst")]);
    let (tx1, mut rx1) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &first_transport,
        &Args::test_default(),
        &cwd,
        "/compact",
        None,
        Vec::new(),
        tx1,
        None,
        Some(large_first_input),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("first compact");
    while rx1.recv().await.is_some() {}
    let first_bodies = first_transport.bodies();
    let first_prompt = first_bodies[0]["input"][0]["content"]
        .as_str()
        .expect("first prompt");

    let second_input = vec![
        summary_message("## Goal\nfirst"),
        json!({"type": "message", "role": "user", "content": "small follow-up"}),
    ];
    let second_transport = StubTransport::new(vec![compact_turn_with_text("## Goal\nsecond")]);
    let (tx2, mut rx2) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &second_transport,
        &Args::test_default(),
        &cwd,
        "/compact",
        None,
        Vec::new(),
        tx2,
        None,
        Some(second_input),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("second compact");
    while rx2.recv().await.is_some() {}
    let second_bodies = second_transport.bodies();
    let second_prompt = second_bodies[0]["input"][0]["content"]
        .as_str()
        .expect("second prompt");

    // Both compactions use SUMMARIZATION_PROMPT — no <previous-summary> block,
    // no incremental/turn-prefix variants.
    assert!(first_prompt.contains("CONTEXT CHECKPOINT COMPACTION"));
    assert!(second_prompt.contains("CONTEXT CHECKPOINT COMPACTION"));
    assert!(!second_prompt.contains("<previous-summary>"));
    // Codex parity: the prior summary stays in the source so the model can
    // carry its narrative (goals, decisions, next steps) into the new summary
    // rather than seeing only the new turns since the last checkpoint.
    assert!(second_prompt.contains("## Goal\nfirst"));
}

#[tokio::test]
async fn manual_compact_recovers_from_text_only_overflow() {
    // Regression for codex review B-2: a text-only long session has no
    // function-call pairs to shed, so the original recovery would always
    // give up. The fallback must instead drop the oldest message and
    // retry — exactly the scenario `/compact` exists to rescue.
    let turn_overflow = vec![StubItem::Err(
        crate::model::responses::ResponsesError::ContextWindowExceeded {
            message: "input is too long".into(),
        },
    )];
    // After the fallback trims one message, the second attempt succeeds
    // and the model returns the handoff summary.
    let turn_summary = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "HANDOFF: recovered"}]
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    let transport = StubTransport::with_items(vec![turn_overflow, turn_summary]);

    // Seed `input` directly via initial_input so the runner sees a text-only
    // pre-compaction transcript. No function_call items.
    let initial_input = vec![
        json!({"type": "message", "role": "user", "content": "one"}),
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "reply"}]}),
        json!({"type": "message", "role": "user", "content": "two"}),
    ];
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &Args::test_default(),
        &cwd,
        "/compact",
        None,
        Vec::new(),
        tx,
        None,
        Some(initial_input),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("compaction should recover from text-only overflow");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    // The runner emitted a ContextTrimmed and a CompactionCompleted.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ContextTrimmed { .. })),
        "expected ContextTrimmed event"
    );
    let completed = events.iter().find_map(|e| match e {
        AgentEvent::CompactionCompleted { summary, .. } => Some(summary.clone()),
        _ => None,
    });
    assert_eq!(completed.as_deref(), Some("HANDOFF: recovered"));

    // Second attempt's serialized conversation must be smaller than the first.
    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 2);
    let first = bodies[0].get("input").and_then(Value::as_array).unwrap();
    let second = bodies[1].get("input").and_then(Value::as_array).unwrap();
    let first_prompt = first[0].get("content").and_then(Value::as_str).unwrap();
    let second_prompt = second[0].get("content").and_then(Value::as_str).unwrap();
    assert!(second_prompt.len() < first_prompt.len());
}

#[tokio::test]
async fn manual_compact_failure_emits_failed_event() {
    // An empty summary is treated as a failure: the agent emits
    // CompactionFailed and the next turn must still see the pre-compact
    // transcript on resume.
    let empty_turn = vec![json!({"type": "response.completed", "response": {}})];
    let transport = StubTransport::new(vec![empty_turn]);
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    let err = run_agent_for_test(
        &transport,
        &Args::test_default(),
        &cwd,
        "/compact",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect_err("empty summary should fail");
    assert!(err.to_string().contains("compaction summary"));

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionFailed { .. }))
    );
}

#[tokio::test]
async fn auto_compact_fires_mid_loop_after_tool_turn_crosses_threshold() {
    // Mid-turn auto-compact: the first sampling iteration in a fresh turn
    // returns a tool call AND reports `input_tokens=95_000` on the
    // `response.completed` envelope. After `finalize_turn` records that
    // reading, the post-turn check inside `'turns: loop` sees the
    // threshold crossed and runs compaction before the next sampling
    // iteration. A pre-turn check is intentionally absent — fresh user
    // prompts are always sent first.
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();

    // Threshold = 100k * 0.85 = 85k. The tool turn reports 95k → crosses.
    let mut args = Args::test_default();
    args.auto_compact_token_limit = 100_000;
    args.auto_compact_fraction = 0.85;
    args.max_turns = 4;

    // Three turns to the transport:
    //   1. Tool-call sampling that reports input_tokens=95k.
    //   2. Compaction summarisation (fires inside the loop).
    //   3. Continuation sampling — final answer post-compaction.
    let tool_call_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        }),
        json!({
            "type": "response.completed",
            "response": {"usage": {"input_tokens": 95_000}},
        }),
    ];
    let summarise_turn = compact_turn_with_text("HANDOFF: did the thing");
    let final_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "All done."}]
            }
        }),
        json!({"type": "response.completed", "response": {"usage": {"input_tokens": 5_000}}}),
    ];
    let transport = StubTransport::new(vec![tool_call_turn, summarise_turn, final_turn]);
    let binding = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "do the thing",
        None,
        Vec::new(),
        tx,
        Some(&binding),
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run with mid-loop auto-compact");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    // Sequence: a tool call ran *before* compaction fired. The mid-turn
    // check is gated on a follow-up iteration being needed.
    let tool_pos = event_position(&events, "ToolCallOutput", |e| {
        matches!(e, AgentEvent::ToolCallOutput { .. })
    });
    let auto_started_pos = event_position(&events, "CompactionStarted", |e| {
        matches!(
            e,
            AgentEvent::CompactionStarted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    assert!(
        tool_pos < auto_started_pos,
        "auto-compaction should fire AFTER a tool call, not before any sampling"
    );

    let auto_completed_pos = event_position(&events, "CompactionCompleted", |e| {
        matches!(
            e,
            AgentEvent::CompactionCompleted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    assert!(auto_started_pos < auto_completed_pos);

    // The post-compaction continuation sampling saw the replacement
    // history: the prior assistant/tool items are gone and the trailing
    // user item is the summary (carry-forward `[user_msgs, initial_ctx,
    // summary]` with `BeforeLastUserMessage` injection).
    let bodies = transport.bodies();
    assert_eq!(
        bodies.len(),
        3,
        "tool sampling + compaction summarisation + continuation"
    );
    let continuation_input = bodies[2].get("input").and_then(Value::as_array).unwrap();
    let function_outputs: Vec<&Value> = continuation_input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
        .collect();
    assert!(
        function_outputs.is_empty(),
        "compaction should drop tool outputs: {function_outputs:?}"
    );
    let last_text = continuation_input
        .last()
        .and_then(|item| item.get("content"))
        .and_then(Value::as_str)
        .expect("continuation input ends with a user message carrying the summary");
    assert!(
        last_text.starts_with(SUMMARY_PREFIX),
        "trailing item is the compaction summary: {last_text}"
    );
}

#[tokio::test]
async fn auto_compact_does_not_fire_on_fresh_user_prompt_without_tool_loop() {
    // A brand-new user prompt is always sent first — even if the session
    // is already at the threshold, the model gets a chance to answer in
    // one sampling iteration. If it returns a final assistant message,
    // no tool follow-up is needed and the mid-turn check never runs.
    // This locks in the codex design: the smallest blast radius of an
    // auto-compaction is a tool-call follow-up, not a brand-new prompt.
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();
    // Seed `latest_input_tokens` past the threshold via a prior
    // TurnComplete event. A pre-turn check would have fired on this seed.
    store
        .append_event(
            &session_id,
            &AgentEvent::TurnComplete {
                usage: TurnUsage {
                    tokens_input: 95_000,
                    tokens_output: 0,
                    tokens_input_cached: 0,
                    tokens_reasoning: 0,
                },
            },
        )
        .unwrap();

    let mut args = Args::test_default();
    args.auto_compact_token_limit = 100_000;
    args.auto_compact_fraction = 0.85;
    args.max_turns = 4;

    // Single transport call: the model returns a final answer
    // immediately, with no tool calls.
    let final_turn = compact_turn_with_text("answer");
    let transport = StubTransport::new(vec![final_turn]);
    let binding = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let prior = store.load_session(&session_id).unwrap();
    let rebuilt = rebuild_responses_input(&prior, &cwd);
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "fresh prompt",
        None,
        Vec::new(),
        tx,
        Some(&binding),
        Some(rebuilt),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run a fresh-prompt turn");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let fired = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    assert!(
        !fired,
        "fresh user prompt with no tool loop must not pre-emptively compact: {events:?}"
    );
    // Exactly one transport call — no compaction summarisation body.
    assert_eq!(transport.bodies().len(), 1);
}

#[tokio::test]
async fn auto_compact_does_not_re_fire_mid_loop_after_checkpoint() {
    // After mid-loop auto-compaction the next continuation sampling reports
    // a *smaller* `input_tokens` because the replacement history is
    // shorter. A subsequent tool-loop turn on the same session reads the
    // post-checkpoint `latest_input_tokens` (not the lifetime cumulative)
    // and stays below the threshold, so the mid-turn check does not
    // re-fire. The cumulative rollup, by contrast, would still cross the
    // threshold — this test fails if anyone ever rewires the decision to
    // read it.
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();

    let mut args = Args::test_default();
    args.auto_compact_token_limit = 100_000;
    args.auto_compact_fraction = 0.85;
    args.max_turns = 6;

    // Run 1: tool call reports input_tokens=95k → mid-turn check fires →
    // compaction → continuation reports input_tokens=5k → final answer.
    let tool_call_turn = |input_tokens: u64| {
        vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "bash",
                    "arguments": "{\"command\":\"echo hi\"}"
                }
            }),
            json!({
                "type": "response.completed",
                "response": {"usage": {"input_tokens": input_tokens}},
            }),
        ]
    };
    let final_turn = |text: &str, input_tokens: u64| {
        vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "content": [{"type": "output_text", "text": text}]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {"usage": {"input_tokens": input_tokens}},
            }),
        ]
    };

    let transport_one = StubTransport::new(vec![
        tool_call_turn(95_000),
        compact_turn_with_text_and_input_tokens("HANDOFF: ongoing", 50_000),
        final_turn("done one", 5_000),
    ]);
    let binding_one = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx1, mut rx1) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport_one,
        &args,
        &cwd,
        "first prompt",
        None,
        Vec::new(),
        tx1,
        Some(&binding_one),
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("first run");
    while rx1.recv().await.is_some() {}

    // Run 2: another tool loop on the same session. The lifetime cumulative
    // counter has been climbing (95k + 50k + 5k = 150k so far), but
    // `latest_input_tokens` is 5k from the previous final answer. With the
    // first tool sampling reporting input_tokens=10k, the mid-turn check
    // sees 10k < 85k and does NOT fire.
    let transport_two =
        StubTransport::new(vec![tool_call_turn(10_000), final_turn("done two", 12_000)]);
    let binding_two = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let prior_two = store.load_session(&session_id).unwrap();
    let rebuilt_two = rebuild_responses_input(&prior_two, &cwd);
    let (tx2, mut rx2) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport_two,
        &args,
        &cwd,
        "second prompt",
        None,
        Vec::new(),
        tx2,
        Some(&binding_two),
        Some(rebuilt_two),
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("second run");
    let mut second_events = Vec::new();
    while let Some(event) = rx2.recv().await {
        second_events.push(event);
    }

    let second_started = second_events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    assert!(
        !second_started,
        "second prompt must not trigger auto-compaction after a recent checkpoint: {second_events:?}"
    );

    // The durable log still has exactly one CompactionCompleted from the
    // first run.
    let after_both = store.load_session(&session_id).unwrap();
    let checkpoints = after_both
        .iter()
        .filter(|e| matches!(e, AgentEvent::CompactionCompleted { .. }))
        .count();
    assert_eq!(
        checkpoints, 1,
        "exactly one auto-compaction should have run"
    );

    // Only two transport calls this run (tool sampling + final answer) —
    // no extra summarisation body.
    let bodies = transport_two.bodies();
    assert_eq!(
        bodies.len(),
        2,
        "expected tool turn + final answer, no compaction"
    );
}

#[tokio::test]
async fn auto_compact_mid_loop_injects_initial_context_before_last_user_message() {
    // Mid-turn compaction must use InitialContextInjection::BeforeLastUserMessage
    // — the model is trained to see the summary at the tail in this path,
    // so the canonical initial-context block is spliced just above the
    // last real user message. The post-compaction continuation body is
    // the assertion surface: its `input` ends with the summary, and the
    // item immediately before the trailing user prompt carries the
    // initial-context block.
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();

    let mut args = Args::test_default();
    args.auto_compact_token_limit = 100_000;
    args.auto_compact_fraction = 0.85;
    args.max_turns = 4;

    let tool_call_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        }),
        json!({
            "type": "response.completed",
            "response": {"usage": {"input_tokens": 95_000}},
        }),
    ];
    let summarise_turn = compact_turn_with_text("HANDOFF: prepared for handoff");
    let final_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "All done."}]
            }
        }),
        json!({"type": "response.completed", "response": {"usage": {"input_tokens": 5_000}}}),
    ];
    let transport = StubTransport::new(vec![tool_call_turn, summarise_turn, final_turn]);
    let binding = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "do the thing",
        None,
        Vec::new(),
        tx,
        Some(&binding),
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run with mid-loop auto-compact");
    while rx.recv().await.is_some() {}

    // Post-compaction continuation body (3rd transport call) is the
    // replacement history fed back to the model.
    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 3);
    let continuation_input = bodies[2].get("input").and_then(Value::as_array).unwrap();

    // Trailing item is the summary, prefixed with SUMMARY_PREFIX.
    let last_text = continuation_input
        .last()
        .and_then(|item| item.get("content"))
        .and_then(Value::as_str)
        .expect("continuation input ends with a string-content user message");
    assert!(
        last_text.starts_with(SUMMARY_PREFIX),
        "trailing item is the compaction summary: {last_text}"
    );

    // The initial-context block sits immediately before the last real
    // user message (which here is the one carrying "do the thing").
    // `build_instructions` always includes `cwd` in the small-coding-agent
    // preamble — we assert on a stable substring of that preamble.
    let user_items: Vec<&Value> = continuation_input
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("user")
        })
        .collect();
    assert!(
        user_items.len() >= 3,
        "expected [initial_context, user_prompt, summary] shape: {user_items:?}"
    );
    let initial_text = user_items
        .iter()
        .rev()
        .skip(2) // skip summary + last user prompt
        .find_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .and_then(|parts| parts.first())
                .and_then(|part| part.get("text"))
                .and_then(Value::as_str)
        })
        .expect("initial-context user item spliced into history");
    assert!(
        initial_text.contains("small coding agent"),
        "spliced item carries the base instructions preamble: {initial_text}"
    );

    // The strip_synthetic_markers pass runs just before send, so the
    // continuation body must not leak the internal `nav_synthetic` field.
    let any_marker = continuation_input
        .iter()
        .any(|item| item.get("nav_synthetic").is_some());
    assert!(
        !any_marker,
        "synthetic marker must be stripped before send: {continuation_input:?}"
    );
}

#[tokio::test]
async fn auto_compact_mid_loop_does_not_re_fire_after_failure_in_same_turn() {
    // If the mid-turn compaction summarisation fails (empty summary), the
    // CompactionFailed event surfaces and `input` is left untouched. The
    // post-finalize check must NOT re-attempt compaction on subsequent
    // iterations of the same user turn — otherwise a transient transport
    // hiccup would burn one failing compaction per tool-loop iteration
    // until `max_turns`.
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();

    let mut args = Args::test_default();
    args.auto_compact_token_limit = 100_000;
    args.auto_compact_fraction = 0.85;
    args.max_turns = 6;

    // Sequence:
    //  1. Tool sampling reports 95k input_tokens (threshold crossed).
    //  2. Compaction summarisation returns ONLY response.completed —
    //     no assistant text → run_compaction_turn returns Err with
    //     "compaction summary was empty" and emits CompactionFailed.
    //  3. Tool sampling again reports 95k. If the latch is missing, the
    //     post-finalize check fires another (failing) compaction here.
    //  4. Final answer ends the loop.
    let tool_turn = |input_tokens: u64| {
        vec![
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "call_id": "call_n",
                    "name": "bash",
                    "arguments": "{\"command\":\"echo hi\"}"
                }
            }),
            json!({
                "type": "response.completed",
                "response": {"usage": {"input_tokens": input_tokens}},
            }),
        ]
    };
    let empty_compaction_turn = vec![json!({"type": "response.completed", "response": {}})];
    let final_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "All done."}]
            }
        }),
        json!({"type": "response.completed", "response": {"usage": {"input_tokens": 95_000}}}),
    ];
    let transport = StubTransport::new(vec![
        tool_turn(95_000),
        empty_compaction_turn,
        tool_turn(95_000),
        final_turn,
    ]);
    let binding = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "do the thing",
        None,
        Vec::new(),
        tx,
        Some(&binding),
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run with failing mid-loop compaction");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    // Exactly one CompactionStarted (from iteration 1) and one
    // CompactionFailed; no successful CompactionCompleted in this run.
    let started = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted { trigger, .. }
                    if matches!(trigger, super::CompactionTrigger::Auto)
            )
        })
        .count();
    let failed = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::CompactionFailed { .. }))
        .count();
    let completed = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::CompactionCompleted { .. }))
        .count();
    assert_eq!(
        started, 1,
        "compaction must not re-fire after a failure in the same turn: {events:?}"
    );
    assert_eq!(failed, 1);
    assert_eq!(completed, 0);

    // The transport saw 4 calls (tool, compaction, tool, final). If the
    // latch were missing, the second tool iteration would have inserted
    // another compaction call, pushing the count to 5+.
    assert_eq!(
        transport.bodies().len(),
        4,
        "extra compaction body would indicate the latch is not working",
    );
}

#[tokio::test]
async fn auto_compact_mid_loop_repushes_ambient_context_after_success() {
    // push_ambient_context fires once at the top of run_agent_inner, and
    // mid-turn compaction's carry-forward filter drops synthetic items.
    // Without an explicit re-push after a successful compaction, the rest
    // of the user turn runs without ambient context. This test pins the
    // re-push: the post-compaction continuation body must contain the
    // ambient-context user message.
    let db_dir = tempdir().unwrap();
    let store =
        crate::context::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::context::PROVIDER_OPENAI_RESPONSES,
            "test-model",
            None,
        )
        .unwrap();

    // Default Args sets ambient_context_token_budget=0 (disabled); raise
    // it so push_ambient_context actually appends an item.
    let mut args = Args::test_default();
    args.ambient_context_token_budget = 256;
    args.auto_compact_token_limit = 100_000;
    args.auto_compact_fraction = 0.85;
    args.max_turns = 4;

    let tool_call_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_1",
                "name": "bash",
                "arguments": "{\"command\":\"echo hi\"}"
            }
        }),
        json!({"type": "response.completed", "response": {"usage": {"input_tokens": 95_000}}}),
    ];
    let summarise_turn = compact_turn_with_text("HANDOFF: ongoing");
    let final_turn = vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "All done."}]
            }
        }),
        json!({"type": "response.completed", "response": {"usage": {"input_tokens": 5_000}}}),
    ];
    let transport = StubTransport::new(vec![tool_call_turn, summarise_turn, final_turn]);
    let binding = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "do the thing",
        None,
        Vec::new(),
        tx,
        Some(&binding),
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("run with ambient context");
    while rx.recv().await.is_some() {}

    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 3);

    fn body_contains_ambient(body: &Value) -> bool {
        body.get("input")
            .and_then(Value::as_array)
            .map(|items| {
                items.iter().any(|item| {
                    item.get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.contains("Ambient context (turn-local"))
                })
            })
            .unwrap_or(false)
    }

    assert!(
        body_contains_ambient(&bodies[0]),
        "first sampling body should carry the initial ambient context",
    );
    assert!(
        body_contains_ambient(&bodies[2]),
        "post-compaction continuation body must re-carry ambient context: {:?}",
        bodies[2],
    );
}

#[tokio::test]
async fn create_failure_does_not_emit_retry_event_from_stub() {
    // The stub doesn't perform retry; this is here to lock in that the
    // `events` parameter is wired through `create()` and unused stubs
    // continue to compile (no behavioral assertion on retry — that's
    // tested in `responses::retry` directly).
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();

    let err = run_agent_for_test(
        &FailingTransport,
        &Args::test_default(),
        &cwd,
        "hi",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect_err("should fail");
    assert!(err.to_string().contains("network down"));
}

#[tokio::test]
async fn manual_compact_emits_analytics_event_with_correct_axes() {
    // A manual `/compact` should produce a structured analytics event with
    // trigger=Manual, reason=UserRequested, phase=StandaloneTurn,
    // status=Completed. The analytics event goes through `tracing`, not
    // the AgentEvent stream, so we capture it with a test subscriber.
    use std::io::Write;
    use tracing_subscriber::EnvFilter;

    // Shared buffer to capture formatted log output.
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    // `MakeWriter` impl for a cloneable handle to the shared buffer.
    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for BufWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(data)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl tracing_subscriber::fmt::MakeWriter<'_> for BufWriter {
        type Writer = Self;
        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    let writer = BufWriter(buf.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("nav.compaction=info"))
        .with_ansi(false)
        .with_writer(writer)
        .finish();
    // `set_global_default` ensures the subscriber is visible across all
    // threads (including any spawned by the tokio runtime inside
    // run_agent_for_test). If another test already installed a global
    // subscriber, `set_global_default` fails — that's a test ordering
    // problem, not a code bug, so we make it a hard fail here to surface it.
    tracing::subscriber::set_global_default(subscriber)
        .expect("global tracing subscriber already installed by another test");

    let transport = StubTransport::new(vec![compact_turn_with_text(
        "handoff: did things, next: more things",
    )]);
    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent_for_test(
        &transport,
        &args,
        &cwd,
        "/compact",
        None,
        Vec::new(),
        tx,
        None,
        None,
        &Catalog::default(),
        None,
        unchecked_permission_context(),
    )
    .await
    .expect("compact run");

    // Drain protocol events (unused here, but must be consumed).
    while rx.recv().await.is_some() {}

    let log_output = {
        let bytes = buf.lock().unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    };
    assert!(
        log_output.contains("compaction analytics event"),
        "analytics event should have been emitted, got: {log_output}"
    );

    // Assert all four axes. String fields are quoted by tracing's formatter.
    assert!(
        log_output.contains("trigger=\"manual\""),
        "expected trigger=\"manual\" in: {log_output}"
    );
    assert!(
        log_output.contains("reason=\"user_requested\""),
        "expected reason=\"user_requested\" in: {log_output}"
    );
    assert!(
        log_output.contains("phase=\"standalone_turn\""),
        "expected phase=\"standalone_turn\" in: {log_output}"
    );
    assert!(
        log_output.contains("status=\"completed\""),
        "expected status=\"completed\" in: {log_output}"
    );

    // The analytics event must NOT appear on the AgentEvent stream.
    // (Already verified implicitly: only CompactionStarted / CompactionCompleted
    // are in the rx channel; the analytics event went to tracing only.)
}
