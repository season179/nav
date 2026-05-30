use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

use nav_harness::compaction::COMPACTION_REPLAY_TEXT;
use nav_harness::compaction::summary::{
    CompactionSummaryAgent, CompactionSummaryRequest, build_compaction_summary_request,
};
use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelRef, ModelResolver, ModelSettings,
    OpenAiCompletionsError, ProviderConfig,
};
use nav_harness::sessions::{CompactionConfig, ModelTurn, SessionStore, ToolCall};
use nav_types::{MessageId, RunId, SessionId};

#[test]
fn summary_request_uses_structured_template_for_realistic_conversation() {
    let summary_request = CompactionSummaryRequest {
        previous_summary: None,
        head_turns: vec![
            ModelTurn::user_text("Work on issue #362 using TDD."),
            ModelTurn::assistant_text("I inspected the issue and found CMP-05b."),
            ModelTurn::user_text("Keep the summary incremental."),
            ModelTurn::assistant_text("I added the first failing test."),
        ],
        tail_start_id: None,
    };
    let request = build_compaction_summary_request(&summary_request);

    assert_eq!(request.messages.len(), 2);
    assert_eq!(request.max_tokens, Some(1_200));
    assert_eq!(request.temperature, Some(0.2));
    assert!(!request.stream);

    let prompt = request.messages[1]
        .content
        .as_ref()
        .expect("user prompt should have content")
        .as_str()
        .expect("user prompt content should be text");

    for section in [
        "## Active Task",
        "## Goal",
        "## Constraints & Preferences",
        "## Completed Actions",
        "## Active State",
        "## Files",
        "### Read",
        "### Modified",
        "## In Progress",
        "## Blocked",
        "## Key Decisions",
    ] {
        assert!(
            prompt.contains(section),
            "summary prompt should include section {section}"
        );
    }

    assert!(prompt.contains("Work on issue #362 using TDD."));
    assert!(prompt.contains("I added the first failing test."));
    assert!(prompt.contains("Write every section with concrete, non-empty content."));
}

#[test]
fn summary_prompt_includes_files_read_from_tool_calls() {
    let summary_request = CompactionSummaryRequest {
        previous_summary: None,
        head_turns: vec![
            ModelTurn::user_text("Read the config file."),
            ModelTurn::assistant_tool_calls(vec![ToolCall {
                id: "tc1".to_string(),
                tool_call_id: None,
                name: "read".to_string(),
                arguments: "{\"path\":\"src/config.rs\"}".to_string(),
            }]),
            ModelTurn::tool_result("tc1", "fn main() {}"),
        ],
        tail_start_id: None,
    };
    let request = build_compaction_summary_request(&summary_request);
    let prompt = request.messages[1]
        .content
        .as_ref()
        .expect("user prompt should have content")
        .as_str()
        .expect("user prompt content should be text");

    let cumulative_read = extract_cumulative_section(prompt, "Read")
        .expect("prompt should have cumulative Read section");
    assert!(
        cumulative_read.contains("src/config.rs"),
        "cumulative Read should list src/config.rs but was:\n{cumulative_read}"
    );
}

#[test]
fn summary_prompt_includes_files_written_from_tool_calls() {
    let summary_request = CompactionSummaryRequest {
        previous_summary: None,
        head_turns: vec![
            ModelTurn::assistant_tool_calls(vec![ToolCall {
                id: "tc1".to_string(),
                tool_call_id: None,
                name: "write".to_string(),
                arguments: "{\"path\":\"src/lib.rs\",\"content\":\"fn main() {}\"}".to_string(),
            }]),
            ModelTurn::tool_result("tc1", "written"),
            ModelTurn::assistant_tool_calls(vec![ToolCall {
                id: "tc2".to_string(),
                tool_call_id: None,
                name: "edit".to_string(),
                arguments: "{\"path\":\"src/config.rs\",\"old\":\"a\",\"new\":\"b\"}".to_string(),
            }]),
            ModelTurn::tool_result("tc2", "edited"),
        ],
        tail_start_id: None,
    };
    let request = build_compaction_summary_request(&summary_request);
    let prompt = request.messages[1]
        .content
        .as_ref()
        .expect("user prompt should have content")
        .as_str()
        .expect("user prompt content should be text");

    let cumulative_modified = extract_cumulative_section(prompt, "Modified")
        .expect("prompt should have cumulative Modified section");
    assert!(
        cumulative_modified.contains("src/lib.rs"),
        "cumulative Modified should list src/lib.rs but was:\n{cumulative_modified}"
    );
    assert!(
        cumulative_modified.contains("src/config.rs"),
        "cumulative Modified should list src/config.rs but was:\n{cumulative_modified}"
    );
}

#[test]
fn summary_prompt_merges_previous_files_with_new_files() {
    let previous_summary = r#"## Files
### Read
src/old.rs

### Modified
src/lib.rs"#;

    let summary_request = CompactionSummaryRequest {
        previous_summary: Some(previous_summary.to_string()),
        head_turns: vec![
            ModelTurn::assistant_tool_calls(vec![ToolCall {
                id: "tc1".to_string(),
                tool_call_id: None,
                name: "read".to_string(),
                arguments: "{\"path\":\"src/new.rs\"}".to_string(),
            }]),
            ModelTurn::tool_result("tc1", "content"),
        ],
        tail_start_id: None,
    };
    let request = build_compaction_summary_request(&summary_request);
    let prompt = request.messages[1]
        .content
        .as_ref()
        .expect("user prompt should have content")
        .as_str()
        .expect("user prompt content should be text");

    let cumulative_read = extract_cumulative_section(prompt, "Read")
        .expect("prompt should have cumulative Read section");
    assert!(
        cumulative_read.contains("src/old.rs"),
        "cumulative Read should carry forward src/old.rs but was:\n{cumulative_read}"
    );
    assert!(
        cumulative_read.contains("src/new.rs"),
        "cumulative Read should include new src/new.rs but was:\n{cumulative_read}"
    );

    let cumulative_modified = extract_cumulative_section(prompt, "Modified")
        .expect("prompt should have cumulative Modified section");
    assert!(
        cumulative_modified.contains("src/lib.rs"),
        "cumulative Modified should carry forward src/lib.rs but was:\n{cumulative_modified}"
    );
}

#[test]
fn summary_prompt_shows_none_when_previous_summary_has_no_files_section() {
    let summary_request = CompactionSummaryRequest {
        previous_summary: Some(summary_a()),
        head_turns: vec![ModelTurn::user_text("do something")],
        tail_start_id: None,
    };
    let request = build_compaction_summary_request(&summary_request);
    let prompt = request.messages[1]
        .content
        .as_ref()
        .expect("user prompt should have content")
        .as_str()
        .expect("user prompt content should be text");

    let cumulative_read = extract_cumulative_section(prompt, "Read")
        .expect("prompt should have cumulative Read section");
    assert_eq!(
        cumulative_read, "None",
        "cumulative Read should be None when previous summary has no Files section"
    );
    let cumulative_modified = extract_cumulative_section(prompt, "Modified")
        .expect("prompt should have cumulative Modified section");
    assert_eq!(
        cumulative_modified, "None",
        "cumulative Modified should be None when previous summary has no Files section"
    );
}

#[test]
fn summary_prompt_does_not_duplicate_files_from_previous_summary() {
    let previous_summary = r#"## Files
### Read
src/config.rs

### Modified
src/lib.rs"#;

    // Same file appears in both previous summary and new tool calls
    let summary_request = CompactionSummaryRequest {
        previous_summary: Some(previous_summary.to_string()),
        head_turns: vec![
            ModelTurn::assistant_tool_calls(vec![ToolCall {
                id: "tc1".to_string(),
                tool_call_id: None,
                name: "read".to_string(),
                arguments: "{\"path\":\"src/config.rs\"}".to_string(),
            }]),
            ModelTurn::tool_result("tc1", "content"),
            ModelTurn::assistant_tool_calls(vec![ToolCall {
                id: "tc2".to_string(),
                tool_call_id: None,
                name: "write".to_string(),
                arguments: "{\"path\":\"src/lib.rs\",\"content\":\"x\"}".to_string(),
            }]),
            ModelTurn::tool_result("tc2", "written"),
        ],
        tail_start_id: None,
    };
    let request = build_compaction_summary_request(&summary_request);
    let prompt = request.messages[1]
        .content
        .as_ref()
        .expect("user prompt should have content")
        .as_str()
        .expect("user prompt content should be text");

    let cumulative_read = extract_cumulative_section(prompt, "Read")
        .expect("prompt should have cumulative Read section");
    let read_count = cumulative_read.matches("src/config.rs").count();
    assert_eq!(
        read_count, 1,
        "src/config.rs should appear once in cumulative Read but appeared {read_count} times:\n{cumulative_read}"
    );

    let cumulative_modified = extract_cumulative_section(prompt, "Modified")
        .expect("prompt should have cumulative Modified section");
    let modified_count = cumulative_modified.matches("src/lib.rs").count();
    assert_eq!(
        modified_count, 1,
        "src/lib.rs should appear once in cumulative Modified but appeared {modified_count} times:\n{cumulative_modified}"
    );
}

/// Extract the content under `### {name}` in the "Cumulative file tracking" section,
/// skipping the template's placeholder version that comes earlier in the prompt.
fn extract_cumulative_section<'a>(prompt: &'a str, name: &str) -> Option<&'a str> {
    let marker = "Cumulative file tracking";
    let tracking_start = prompt.find(marker)?;
    let tracking = &prompt[tracking_start..];
    let heading = format!("### {name}");
    let heading_start = tracking.find(&heading)?;
    let after_heading = &tracking[heading_start + heading.len()..];
    let end = after_heading.find("\n##").unwrap_or(after_heading.len());
    Some(after_heading[..end].trim())
}

#[test]
fn two_pass_compaction_feeds_previous_summary_and_stores_incremental_summary() {
    let store = SessionStore::default();
    let session_id = session_id();
    let first_run_id = run_id(1);
    let second_run_id = run_id(2);
    let summary_a = summary_a();
    let summary_b = summary_b();

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, first_run_id.clone()).unwrap();

    for index in 0..10 {
        let message_id = message_id(index);
        let turn = if index % 2 == 0 {
            ModelTurn::user_text(format!("user {index}"))
        } else {
            ModelTurn::assistant_text(format!("assistant {index}"))
        };
        store.append_turn(&first_run_id, message_id, turn).unwrap();
    }

    let first_summary_request = store
        .compaction_summary_request(&session_id, CompactionConfig::default())
        .unwrap();
    store
        .compact_session_with_summary(&session_id, &first_summary_request, summary_a.clone())
        .unwrap();

    store.start_run(&session_id, second_run_id.clone()).unwrap();
    for index in 10..13 {
        let turn = if index % 2 == 0 {
            ModelTurn::user_text(format!("user {index}"))
        } else {
            ModelTurn::assistant_text(format!("assistant {index}"))
        };
        store
            .append_turn(&second_run_id, message_id(index), turn)
            .unwrap();
    }

    let second_summary_request = store
        .compaction_summary_request(&session_id, CompactionConfig::default())
        .unwrap();
    let head_text = second_summary_request
        .head_turns
        .iter()
        .map(ModelTurn::text_content)
        .collect::<Vec<_>>();

    assert_eq!(second_summary_request.previous_summary, Some(summary_a));
    assert_eq!(head_text, vec!["user 8", "assistant 9", "user 10"]);

    let second_boundary = store
        .compact_session_with_summary(&session_id, &second_summary_request, summary_b.clone())
        .unwrap();
    assert_eq!(second_boundary.tail_start_id, Some(message_id(11)));

    let replay_text = store
        .try_turns(&session_id)
        .unwrap()
        .iter()
        .map(ModelTurn::text_content)
        .collect::<Vec<_>>();

    assert_eq!(
        replay_text,
        vec![
            COMPACTION_REPLAY_TEXT.to_string(),
            summary_b,
            "assistant 11".to_string(),
            "user 12".to_string(),
        ]
    );
    assert!(replay_text[1].contains("Inspected issue #362 - found CMP-05b"));
}

#[test]
fn summary_write_uses_request_tail_boundary_when_new_turns_arrive() {
    let store = SessionStore::default();
    let session_id = session_id();
    let first_run_id = run_id(1);
    let second_run_id = run_id(2);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, first_run_id.clone()).unwrap();
    for index in 0..6 {
        store
            .append_turn(
                &first_run_id,
                message_id(index),
                ModelTurn::user_text(format!("turn {index}")),
            )
            .unwrap();
    }

    let summary_request = store
        .compaction_summary_request(&session_id, CompactionConfig::default())
        .unwrap();
    assert_eq!(summary_request.tail_start_id, Some(message_id(4)));

    store.start_run(&session_id, second_run_id.clone()).unwrap();
    for index in 6..8 {
        store
            .append_turn(
                &second_run_id,
                message_id(index),
                ModelTurn::user_text(format!("late turn {index}")),
            )
            .unwrap();
    }

    let boundary = store
        .compact_session_with_summary(&session_id, &summary_request, summary_a())
        .unwrap();
    let replay_text = store
        .try_turns(&session_id)
        .unwrap()
        .iter()
        .map(ModelTurn::text_content)
        .collect::<Vec<_>>();

    assert_eq!(boundary.tail_start_id, Some(message_id(4)));
    assert_eq!(
        replay_text,
        vec![
            COMPACTION_REPLAY_TEXT.to_string(),
            summary_a(),
            "turn 4".to_string(),
            "turn 5".to_string(),
            "late turn 6".to_string(),
            "late turn 7".to_string(),
        ]
    );
}

#[test]
fn summary_write_rejects_tail_boundary_from_another_session() {
    let store = SessionStore::default();
    let source_session_id = session_id();
    let target_session_id = session_id_2();
    let source_run_id = run_id(1);
    let target_run_id = run_id(2);

    store.create_session(source_session_id.clone()).unwrap();
    store
        .start_run(&source_session_id, source_run_id.clone())
        .unwrap();
    for index in 0..4 {
        store
            .append_turn(
                &source_run_id,
                message_id(index),
                ModelTurn::user_text(format!("source {index}")),
            )
            .unwrap();
    }
    let source_request = store
        .compaction_summary_request(&source_session_id, CompactionConfig::default())
        .unwrap();

    store.create_session(target_session_id.clone()).unwrap();
    store
        .start_run(&target_session_id, target_run_id.clone())
        .unwrap();
    store
        .append_turn(
            &target_run_id,
            message_id(10),
            ModelTurn::user_text("target"),
        )
        .unwrap();

    let error = store
        .compact_session_with_summary(&target_session_id, &source_request, summary_a())
        .unwrap_err();

    assert!(error.to_string().contains("compaction tail turn not found"));
}

#[test]
fn placeholder_summary_is_not_fed_back_as_previous_summary() {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id(1);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    store
        .append_turn(&run_id, message_id(1), ModelTurn::user_text("first task"))
        .unwrap();

    store
        .compact_session(&session_id, CompactionConfig::default())
        .unwrap();

    let request = store
        .compaction_summary_request(&session_id, CompactionConfig::default())
        .unwrap();

    assert_eq!(request.previous_summary, None);
}

#[test]
fn placeholder_summary_does_not_hide_older_real_summary() {
    let store = SessionStore::default();
    let session_id = session_id();
    let first_run_id = run_id(1);
    let second_run_id = run_id(2);
    let summary_a = summary_a();

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, first_run_id.clone()).unwrap();
    for index in 0..4 {
        store
            .append_turn(
                &first_run_id,
                message_id(index),
                ModelTurn::user_text(format!("first run {index}")),
            )
            .unwrap();
    }

    let first_summary_request = store
        .compaction_summary_request(&session_id, CompactionConfig::default())
        .unwrap();
    store
        .compact_session_with_summary(&session_id, &first_summary_request, summary_a.clone())
        .unwrap();

    store.start_run(&session_id, second_run_id.clone()).unwrap();
    store
        .append_turn(
            &second_run_id,
            message_id(10),
            ModelTurn::user_text("new work after real summary"),
        )
        .unwrap();
    store
        .compact_session(&session_id, CompactionConfig::default())
        .unwrap();

    let request = store
        .compaction_summary_request(&session_id, CompactionConfig::default())
        .unwrap();

    assert_eq!(request.previous_summary, Some(summary_a));
}

#[test]
fn compaction_summary_agent_calls_model_and_returns_assistant_text() {
    let server = SequencedResponseServer::new(vec![SequencedResponse::ok(
        "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"## Active Task\\nGenerated summary\"}}]}",
    )]);
    let model = resolved_model(server.base_url());
    let agent = CompactionSummaryAgent::new();
    let summary_request = CompactionSummaryRequest {
        previous_summary: Some(summary_a()),
        head_turns: vec![ModelTurn::user_text("continue with the next slice")],
        tail_start_id: None,
    };

    let summary = agent.generate(&model, &summary_request).unwrap();

    let request_bodies = server.request_bodies();
    assert_eq!(summary, "## Active Task\nGenerated summary");
    assert_eq!(request_bodies.len(), 1);
    assert!(request_bodies[0].contains("Previous summary"));
    assert!(request_bodies[0].contains("continue with the next slice"));
}

#[test]
fn compaction_call_overflow_drops_oldest_head_turns_and_retries() {
    let overflow_body = "{\"error\":{\"message\":\"This model's maximum context length is exceeded\",\"code\":\"context_length_exceeded\"}}";
    let success_body =
        "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"## Active Task\\nRetried summary\"}}]}";
    let server = SequencedResponseServer::new(vec![
        SequencedResponse::error(400, overflow_body),
        SequencedResponse::ok(success_body),
    ]);
    let model = resolved_model(server.base_url());
    let agent = CompactionSummaryAgent::new();
    let summary_request = CompactionSummaryRequest {
        previous_summary: None,
        head_turns: vec![
            ModelTurn::user_text("OLDEST_HEAD_TURN keep dropping me"),
            ModelTurn::assistant_text("middle head turn"),
            ModelTurn::user_text("NEWEST_HEAD_TURN must survive"),
        ],
        tail_start_id: None,
    };

    let summary = agent.generate(&model, &summary_request).unwrap();

    assert_eq!(summary, "## Active Task\nRetried summary");

    let request_bodies = server.request_bodies();
    assert_eq!(
        request_bodies.len(),
        2,
        "agent should retry exactly once after a single overflow"
    );
    assert!(
        request_bodies[0].contains("OLDEST_HEAD_TURN"),
        "first attempt should include the oldest head turn"
    );
    assert!(
        !request_bodies[1].contains("OLDEST_HEAD_TURN"),
        "retry should drop the oldest head turn but body was:\n{}",
        request_bodies[1]
    );
    assert!(
        request_bodies[1].contains("NEWEST_HEAD_TURN"),
        "retry should keep the newest head turn but body was:\n{}",
        request_bodies[1]
    );
}

#[test]
fn persistent_compaction_overflow_gives_up_after_bounded_retries() {
    let overflow_body = "{\"error\":{\"message\":\"This model's maximum context length is exceeded\",\"code\":\"context_length_exceeded\"}}";
    // More overflow responses than any reasonable bound; the agent must stop
    // well before exhausting them rather than looping forever.
    let server = SequencedResponseServer::new(
        (0..16)
            .map(|_| SequencedResponse::error(400, overflow_body))
            .collect(),
    );
    let model = resolved_model(server.base_url());
    let agent = CompactionSummaryAgent::new();
    let summary_request = CompactionSummaryRequest {
        previous_summary: None,
        head_turns: (0..8)
            .map(|index| ModelTurn::user_text(format!("head turn {index}")))
            .collect(),
        tail_start_id: None,
    };

    let error = agent.generate(&model, &summary_request).unwrap_err();

    assert!(
        matches!(error, OpenAiCompletionsError::ContextLimit(_)),
        "persistent overflow should surface ContextLimit, got: {error:?}"
    );

    let attempts = server.request_bodies().len();
    assert!(
        (2..=5).contains(&attempts),
        "agent should retry a bounded handful of times, made {attempts} attempts"
    );
}

fn session_id() -> SessionId {
    SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap()
}

fn session_id_2() -> SessionId {
    SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000002").unwrap()
}

fn run_id(suffix: u64) -> RunId {
    RunId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}")).unwrap()
}

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f29-{suffix:012x}")).unwrap()
}

fn summary_a() -> String {
    r#"## Active Task
Work on issue #362 using TDD.

## Goal
Implement CMP-05b summarization.

## Constraints & Preferences
Use TDD and keep compaction incremental.

## Completed Actions
1. Inspected issue #362 - found CMP-05b [tool: gh]

## Active State
Prompt builder test is green.

## In Progress
Wiring generated summaries into storage.

## Blocked
None

## Key Decisions
Use the previous summary as explicit model context."#
        .to_string()
}

fn summary_b() -> String {
    r#"## Active Task
Work on issue #362 using TDD.

## Goal
Implement CMP-05b summarization.

## Constraints & Preferences
Use TDD and keep compaction incremental.

## Completed Actions
1. Inspected issue #362 - found CMP-05b [tool: gh]
2. Summarized the next head turn - retained previous completed actions [tool: compaction]

## Active State
Second compaction summary is stored in replay.

## In Progress
Verification.

## Blocked
None

## Key Decisions
Incremental compaction carries prior completed actions forward."#
        .to_string()
}

fn resolved_model(base_url: &str) -> nav_harness::models::ResolvedModelConfig {
    let mut providers = BTreeMap::new();
    providers.insert(
        "test-provider".to_string(),
        ProviderConfig {
            name: None,
            api: ApiKind::OpenAiCompletions,
            base_url: base_url.to_string(),
            api_key: ApiKeyConfig::Inline {
                inline: "test-secret".to_string(),
            },
            models: vec![ModelConfig {
                id: "summary-model".to_string(),
                name: None,
                api: None,
                base_url: None,
                reasoning: false,
                input: Vec::new(),
                context_window: None,
                max_tokens: None,
                compat: Default::default(),
            }],
            compat: Default::default(),
        },
    );

    ModelResolver::new(ModelSettings {
        default_model: Some(ModelRef {
            provider: "test-provider".to_string(),
            model: "summary-model".to_string(),
        }),
        providers,
    })
    .resolve_default()
    .unwrap()
}

struct SequencedResponse {
    status: u16,
    body: &'static str,
}

impl SequencedResponse {
    fn ok(body: &'static str) -> Self {
        Self { status: 200, body }
    }

    fn error(status: u16, body: &'static str) -> Self {
        Self { status, body }
    }
}

/// Mock server that replies with a queued sequence of responses, one per
/// incoming request, capturing every request body for later assertions.
struct SequencedResponseServer {
    base_url: String,
    request_bodies: Receiver<String>,
}

impl SequencedResponseServer {
    fn new(responses: Vec<SequencedResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, request_bodies) = channel();

        thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let body = read_http_request_body(&mut stream);
                sender.send(body).unwrap();

                let reason = if response.status == 200 { "OK" } else { "Bad Request" };
                let http = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.status,
                    reason,
                    response.body.len(),
                    response.body
                );
                stream.write_all(http.as_bytes()).unwrap();
            }
        });

        Self {
            base_url: format!("http://{address}"),
            request_bodies,
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn request_bodies(self) -> Vec<String> {
        let mut bodies = Vec::new();
        while let Ok(body) = self.request_bodies.recv_timeout(Duration::from_secs(2)) {
            bodies.push(body);
        }
        bodies
    }
}

fn read_http_request_body(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];

    loop {
        let read = stream.read(&mut buffer).unwrap();
        bytes.extend_from_slice(&buffer[..read]);

        let Some(header_end) = find_bytes(&bytes, b"\r\n\r\n") else {
            continue;
        };
        let content_length = content_length(&bytes[..header_end]);
        let body_start = header_end + 4;
        if bytes.len().saturating_sub(body_start) >= content_length {
            return String::from_utf8_lossy(&bytes[body_start..body_start + content_length])
                .to_string();
        }
    }
}

fn content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
