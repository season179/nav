use super::*;
use crate::cli::Args;
use crate::control::{PendingInput, PendingInputMode, TurnControls};
use crate::permissions::approval::{ApprovalGate, ApprovalRequest};
use crate::permissions::{AskForApproval, ReviewDecision, SandboxPolicy, SessionAllowlist};
use crate::sandbox::PassthroughRunner;
use crate::skills::Catalog;
use crate::tools::PermissionContext;
use anyhow::Result;
use futures_util::stream;
use serde_json::{Value, json};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};
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
        json!({"type": "message", "role": "user", "content": super::SUMMARIZATION_PROMPT}),
    ];
    let dropped = super::runner::trim_for_compaction(&mut input);
    assert_eq!(dropped, 1);
    assert_eq!(input.len(), 3);
    // The synthesised summarisation prompt is still the last item.
    let last_text = input
        .last()
        .and_then(|v| v.get("content"))
        .and_then(Value::as_str);
    assert_eq!(last_text, Some(super::SUMMARIZATION_PROMPT));
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
        vec![json!({"type": "message", "role": "user", "content": super::SUMMARIZATION_PROMPT})];
    assert_eq!(super::runner::trim_for_compaction(&mut input), 0);
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
        crate::tools::unchecked_permission_context(),
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
        crate::tools::unchecked_permission_context(),
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

    run_agent_with_control(
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
        crate::tools::unchecked_permission_context(),
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

    run_agent(
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
        crate::tools::unchecked_permission_context(),
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
        crate::tools::unchecked_permission_context(),
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
    run_agent(
        &transport,
        &Args::test_default(),
        &cwd,
        "look".into(),
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
    let bodies = transport.bodies.lock().unwrap();
    let serialized = bodies[0].to_string();
    assert!(
        !serialized.contains("hunter2"),
        "denied secret leaked into request: {serialized}"
    );

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
    run_agent(
        &transport,
        &Args::test_default(),
        &cwd,
        "look".into(),
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
    run_agent(
        &transport,
        &Args::test_default(),
        &cwd,
        "look".into(),
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
    let bodies = transport.bodies.lock().unwrap();
    assert!(
        bodies.is_empty(),
        "aborted turn must not call the transport: {bodies:#?}"
    );

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
        crate::tools::unchecked_permission_context(),
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
        crate::tools::unchecked_permission_context(),
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
        crate::tools::unchecked_permission_context(),
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
        crate::tools::unchecked_permission_context(),
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
    let transport =
        StubTransport::with_items(vec![turn_one, turn_two_overflow, turn_three_after_trim]);

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
        crate::tools::unchecked_permission_context(),
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
        crate::tools::unchecked_permission_context(),
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

// ── compaction integration ────────────────────────────────────

/// Build a single-turn stub that returns one assistant message with the
/// given text and a `response.completed` envelope. Used by the compaction
/// tests as the stand-in for "the model wrote a summary."
fn compact_turn_with_text(text: &str) -> Vec<Value> {
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

    run_agent(
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
        crate::tools::unchecked_permission_context(),
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
            AgentEvent::CompactionCompleted { trigger, summary, .. }
                if matches!(trigger, super::CompactionTrigger::Manual)
                    && summary.contains("handoff: did things")
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
    run_agent(
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
        crate::tools::unchecked_permission_context(),
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
    run_agent(
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
        crate::tools::unchecked_permission_context(),
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
    assert!(only_user.starts_with(super::SUMMARY_PREFIX));
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
async fn manual_compact_recovers_from_text_only_overflow() {
    // Regression for codex review B-2: a text-only long session has no
    // function-call pairs to shed, so the original recovery would always
    // give up. The fallback must instead drop the oldest message and
    // retry — exactly the scenario `/compact` exists to rescue.
    let turn_overflow = vec![StubItem::Err(
        crate::responses::ResponsesError::ContextWindowExceeded {
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

    run_agent(
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
        crate::tools::unchecked_permission_context(),
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

    // Second attempt's body must have one fewer item than the first.
    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 2);
    let first = bodies[0].get("input").and_then(Value::as_array).unwrap();
    let second = bodies[1].get("input").and_then(Value::as_array).unwrap();
    assert_eq!(second.len(), first.len() - 1);
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

    let err = run_agent(
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
        crate::tools::unchecked_permission_context(),
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
async fn auto_compact_fires_when_session_tokens_cross_threshold() {
    // Pre-populate a session whose recorded `tokens_input` is above the
    // auto-compact threshold, then submit a normal prompt. The runner
    // should run compaction first, then proceed with the user's turn.
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
    // Seed the rolling token total by pretending an earlier turn consumed
    // 95k input tokens. Append a TurnComplete event so append_event runs
    // the rollup UPDATE.
    store
        .append_event(
            &session_id,
            &AgentEvent::UserMessage {
                text: "huge".into(),
                display_text: None,
                attachments: Vec::new(),
            },
        )
        .unwrap();
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

    // Threshold = 100k * 0.85 = 85k. 95k crosses.
    let mut args = Args::test_default();
    args.auto_compact_token_limit = 100_000;
    args.auto_compact_fraction = 0.85;

    // Two turns to the transport: first is the compaction summarisation,
    // second is the user's actual prompt response.
    let summarise_turn = compact_turn_with_text("HANDOFF: ongoing");
    let user_turn = compact_turn_with_text("ack second");
    let transport = StubTransport::new(vec![summarise_turn, user_turn]);
    let binding = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let prior = store.load_session(&session_id).unwrap();
    let rebuilt = rebuild_responses_input(&prior, &cwd);
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent(
        &transport,
        &args,
        &cwd,
        "regular prompt",
        None,
        Vec::new(),
        tx,
        Some(&binding),
        Some(rebuilt),
        &Catalog::default(),
        None,
        crate::tools::unchecked_permission_context(),
    )
    .await
    .expect("run with auto-compact");

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let auto_started = events.iter().find(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    assert!(auto_started.is_some(), "auto-compaction should have fired");
    let auto_completed = events.iter().find(|e| {
        matches!(
            e,
            AgentEvent::CompactionCompleted { trigger, .. }
                if matches!(trigger, super::CompactionTrigger::Auto)
        )
    });
    assert!(auto_completed.is_some());

    // The user's actual prompt followed, and the second transport call
    // saw the replacement history (summary + recent users) plus the new
    // user prompt — not the raw 95k-token transcript.
    let bodies = transport.bodies();
    assert_eq!(bodies.len(), 2, "auto-compact + user turn");
    let final_input = bodies[1].get("input").and_then(Value::as_array).unwrap();
    let assistants: Vec<&Value> = final_input
        .iter()
        .filter(|item| item.get("role").and_then(Value::as_str) == Some("assistant"))
        .collect();
    assert!(
        assistants.is_empty(),
        "compaction should drop assistant items: {assistants:?}"
    );
    // The new user prompt is the last item in `input`.
    let last = final_input.last().unwrap();
    assert_eq!(
        last.get("content").and_then(Value::as_str),
        Some("regular prompt")
    );
}

#[tokio::test]
async fn auto_compact_does_not_re_fire_after_checkpoint() {
    // Regression for codex review B-3: rolling token totals are lifetime
    // cumulative, so naive threshold checks would re-compact every turn
    // after the first crossing. The runner must instead key off
    // `rolling - latest_checkpoint.tokens_before` so a second prompt
    // submitted shortly after a successful auto-compaction does NOT
    // trigger compaction again.
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
    store
        .append_event(
            &session_id,
            &AgentEvent::UserMessage {
                text: "huge".into(),
                display_text: None,
                attachments: Vec::new(),
            },
        )
        .unwrap();
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

    // First run: auto-compact + user turn.
    let transport_one = StubTransport::new(vec![
        compact_turn_with_text("HANDOFF: ongoing"),
        compact_turn_with_text("ack"),
    ]);
    let binding_one = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let prior_one = store.load_session(&session_id).unwrap();
    let rebuilt_one = rebuild_responses_input(&prior_one, &cwd);
    let (tx1, mut rx1) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent(
        &transport_one,
        &args,
        &cwd,
        "first prompt",
        None,
        Vec::new(),
        tx1,
        Some(&binding_one),
        Some(rebuilt_one),
        &Catalog::default(),
        None,
        crate::tools::unchecked_permission_context(),
    )
    .await
    .expect("first run");
    while rx1.recv().await.is_some() {}

    // Second run: with the lifetime rolling counter still ≥ threshold, a
    // naive implementation would compact again. We assert it does NOT.
    let transport_two = StubTransport::new(vec![compact_turn_with_text("ack two")]);
    let binding_two = SessionBinding {
        store: &store,
        session_id: session_id.clone(),
    };
    let prior_two = store.load_session(&session_id).unwrap();
    let rebuilt_two = rebuild_responses_input(&prior_two, &cwd);
    let (tx2, mut rx2) = mpsc::unbounded_channel::<AgentEvent>();
    run_agent(
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
        crate::tools::unchecked_permission_context(),
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

    // Only one transport call this run (the user turn) — no extra
    // summarisation body.
    let bodies = transport_two.bodies();
    assert_eq!(bodies.len(), 1, "expected single user turn, no compaction");
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
        crate::tools::unchecked_permission_context(),
    )
    .await
    .expect_err("should fail");
    assert!(err.to_string().contains("network down"));
}
