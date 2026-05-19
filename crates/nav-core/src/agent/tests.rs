use super::*;
use crate::cli::Args;
use crate::skills::Catalog;
use anyhow::Result;
use futures_util::stream;
use serde_json::{Value, json};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command as StdCommand;
use std::sync::Mutex;
use tempfile::tempdir;
use tokio::sync::mpsc;

// ── stub transport ────────────────────────────────────────────

/// One unit of stream output a stub can hand back per turn.
///
/// Used by the few tests that need to inject a transport error (e.g. a
/// `context_length_exceeded`) into the middle of the agent loop. The normal
/// "happy path" tests use `StubTransport::new(turns_of_values)`, which lifts
/// `Value`s into `StubItem::Event` automatically.
enum StubItem {
    Event(Value),
    Err(crate::responses::ResponsesError),
}

/// Pops one canned event list per `create()` call so each turn of the
/// agent loop sees the next pre-recorded `Responses` API stream.
struct StubTransport {
    turns: Mutex<Vec<Vec<StubItem>>>,
    bodies: Mutex<Vec<Value>>,
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
        }
    }

    fn with_items(turns: Vec<Vec<StubItem>>) -> Self {
        Self {
            turns: Mutex::new(turns),
            bodies: Mutex::new(Vec::new()),
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

fn git(cwd: &std::path::Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed with {status}");
}

impl ResponsesTransport for StubTransport {
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
fn rebuild_responses_input_skips_tool_events() {
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

// ── run_agent end-to-end ──────────────────────────────────────

#[tokio::test]
async fn run_agent_emits_single_error_when_transport_create_fails() {
    let args = Args::test_default();
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    let err = run_agent(
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
    let result = run_agent(
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
    run_agent(
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
                    && *status == crate::mutation::PatchApplyStatus::Completed
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
    run_agent(
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
                    && *status == crate::mutation::PatchApplyStatus::Failed
                    && summary.contains("note.txt")
                    && error.as_deref().is_some_and(|message| message.contains("tool error"))
        )
    });
    assert_eq!(fs::read_to_string(cwd.join("note.txt")).unwrap(), "old\n");
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
        crate::session::SessionStore::open(Some(db_dir.path().join("nav.db"))).expect("open store");
    let cwd_dir = tempdir().unwrap();
    let cwd = cwd_dir.path().canonicalize().unwrap();
    let session_id = store
        .create_session(
            &cwd,
            crate::session::PROVIDER_OPENAI_RESPONSES,
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
    run_agent(
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
    run_agent(
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
    run_agent(
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

// ── context-overflow recovery ─────────────────────────────────

#[tokio::test]
async fn overflow_one_shot_recovery_trims_and_continues() {
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
        crate::responses::ResponsesError::ContextWindowExceeded {
            message: "input is too long".into(),
        },
    )];
    // Turn 3 (after recovery trims the oldest tool pair): model finishes.
    let turn_three = vec![
        StubItem::Event(json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "content": [{"type": "output_text", "text": "ok"}]
            }
        })),
        StubItem::Event(json!({"type": "response.completed", "response": {}})),
    ];
    let transport = StubTransport::with_items(vec![turn_one, turn_two, turn_three]);

    let mut args = Args::test_default();
    args.max_turns = 6;
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent(
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
    )
    .await
    .expect("recovery should succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    let trimmed = events
        .iter()
        .find(|e| matches!(e, AgentEvent::ContextTrimmed { .. }))
        .expect("expected ContextTrimmed event");
    assert!(matches!(
        trimmed,
        AgentEvent::ContextTrimmed { dropped_pairs } if *dropped_pairs == 1
    ));

    // The recovery-retry body (3rd `create()` call) must no longer contain
    // the original `function_call` for call_1.
    let bodies = transport.bodies();
    assert_eq!(
        bodies.len(),
        3,
        "agent should make exactly 3 transport calls"
    );
    let recovery_input = bodies[2]
        .get("input")
        .and_then(Value::as_array)
        .expect("recovery body has input");
    let has_call_1 = recovery_input.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("function_call")
            && item.get("call_id").and_then(Value::as_str) == Some("call_1")
    });
    assert!(!has_call_1, "call_1 should have been trimmed");

    // Recovery is one-shot; the flag is consumed. No need to assert directly.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AssistantMessageDone { text } if text == "ok"))
    );
}

#[tokio::test]
async fn overflow_recovery_does_not_consume_turn_budget() {
    // With max_turns=2, the agent must still be able to (1) run a tool-call
    // turn, (2) hit overflow, trim, (3) retry, and (4) finish — even though
    // the trim+retry conceptually happens on what would have been the "last"
    // turn. Recovery is bookkeeping, not a real model turn.
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
        crate::responses::ResponsesError::ContextWindowExceeded {
            message: "too long".into(),
        },
    )];
    let turn_three_after_trim = vec![
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
        turn_three_after_trim,
    ]);

    let mut args = Args::test_default();
    args.max_turns = 2;
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    run_agent(
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
    )
    .await
    .expect("recovery on the last allowed turn should still succeed");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ContextTrimmed { dropped_pairs: 1 }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AssistantMessageDone { text } if text == "done"))
    );
    assert_eq!(transport.bodies().len(), 3, "3 transport calls expected");
}

#[tokio::test]
async fn overflow_second_failure_surfaces_clean_error() {
    // Turn 1: tool call to seed a droppable pair.
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
    // First overflow.
    let turn_two = vec![StubItem::Err(
        crate::responses::ResponsesError::ContextWindowExceeded {
            message: "too long".into(),
        },
    )];
    // Second overflow — recovery already consumed, must surface as Error.
    let turn_three = vec![StubItem::Err(
        crate::responses::ResponsesError::ContextWindowExceeded {
            message: "still too long".into(),
        },
    )];
    let transport = StubTransport::with_items(vec![turn_one, turn_two, turn_three]);

    let mut args = Args::test_default();
    args.max_turns = 6;
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    let err = run_agent(
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
    )
    .await
    .expect_err("second overflow should fail");
    assert!(err.to_string().contains("context window exceeded"));

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let trimmed_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ContextTrimmed { .. }))
        .count();
    assert_eq!(trimmed_count, 1, "recovery should only fire once");
    let error_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Error { .. }))
        .count();
    assert_eq!(error_count, 1);
}

// ── transport-level retry plumbing ────────────────────────────

#[tokio::test]
async fn create_failure_does_not_emit_retry_event_from_stub() {
    // The stub doesn't perform retry; this is here to lock in that the
    // `events` parameter is wired through `create()` and unused stubs
    // continue to compile (no behavioral assertion on retry — that's
    // tested in `responses::retry` directly).
    let cwd = tempdir().unwrap();
    let cwd = cwd.path().canonicalize().unwrap();
    let (tx, _rx) = mpsc::unbounded_channel::<AgentEvent>();

    let err = run_agent(
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
    )
    .await
    .expect_err("should fail");
    assert!(err.to_string().contains("network down"));
}
