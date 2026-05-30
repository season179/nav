//! End-to-end run-loop coverage for non-Chat-Completions dialects.
//!
//! Each test drives `RunLoop::run` against a fake provider server that answers
//! one canned HTTP response, proving the live loop selects the right
//! encoder/transport/decoder from the resolved `ApiKind`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nav_harness::agents::{
    PendingTurnLookupResults, PrefetchedTurnContext, RunLoop, RunLoopRequest, RunLoopResult,
    TurnContextPrefetcher, TurnLookupPrefetchRequest, TurnLookupPrefetcher,
};
use nav_harness::events::{HarnessEvent, HarnessEventEnvelope, HarnessEventIdSource};
use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelRef, ModelResolver, ModelSettings,
    OpenAiCompletionsCancellationToken, OpenAiCompletionsClient, ProviderConfig,
    ResolvedModelConfig,
};
use nav_harness::sessions::{ModelTurn, ModelTurnRole, ProviderState, SessionStore, TurnPart};
use nav_harness::tools::{ToolContext, ToolPreset, ToolRegistry, read};
use nav_harness::workspace::path::WorkspacePathPolicy;
use nav_types::{ApprovalId, EventId, MessageId, RunId, SessionId, ToolCallId};

const DOOM_LOOP_READ_MESSAGE: &str = "[doom_loop detected: tool read with identical arguments called 3 times. Try a different approach.]";
const DOOM_LOOP_REPEAT_TOOL_MESSAGE: &str = "[doom_loop detected: tool repeat_tool with identical arguments called 3 times. Try a different approach.]";

#[test]
fn anthropic_messages_run_completes_end_to_end() {
    let body = r#"{
        "id": "msg_01",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "text", "text": "Hello from Anthropic!"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 5, "output_tokens": 3}
    }"#;
    let server = FakeProviderServer::start(vec![CannedResponse::json(body)]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "say hello");

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "anthropic run should complete, got {result:?}"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let assistant = final_turns
        .iter()
        .rev()
        .find(|turn| turn.role == ModelTurnRole::Assistant)
        .expect("an assistant turn should be persisted");
    assert_eq!(assistant.text_content(), "Hello from Anthropic!");
}

#[test]
fn openai_responses_run_completes_end_to_end() {
    let body = r#"{
        "id": "resp_01",
        "model": "gpt-test",
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "Hello from Responses!", "annotations": []}]
        }],
        "usage": {"input_tokens": 7, "output_tokens": 4}
    }"#;
    let server = FakeProviderServer::start(vec![CannedResponse::json(body)]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "say hello");

    let model = responses_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "responses run should complete, got {result:?}"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let assistant = final_turns
        .iter()
        .rev()
        .find(|turn| turn.role == ModelTurnRole::Assistant)
        .expect("an assistant turn should be persisted");
    assert_eq!(assistant.text_content(), "Hello from Responses!");
}

#[test]
fn openai_responses_run_attaches_cached_previous_response_id() {
    let body = r#"{
        "id": "resp_02",
        "model": "gpt-test",
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "continued", "annotations": []}]
        }]
    }"#;
    let server = FakeProviderServer::start(vec![CannedResponse::json(body)]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "continue");
    store
        .lock()
        .unwrap()
        .set_provider_state(ProviderState {
            run_id: run_id.clone(),
            api_kind: "openai-responses".to_string(),
            state_json: r#"{"previous_response_id":"resp_cached"}"#.to_string(),
        })
        .expect("provider state should persist");

    let model = responses_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "responses run should complete, got {result:?}"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "expected one provider request");
    let request: serde_json::Value =
        serde_json::from_str(&requests[0]).expect("request body should be JSON");
    assert_eq!(request["previous_response_id"], "resp_cached");
}

#[test]
fn openai_responses_run_persists_provider_state() {
    let body = r#"{
        "id": "resp_saved",
        "model": "gpt-test",
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "state saved", "annotations": []}]
        }]
    }"#;
    let server = FakeProviderServer::start(vec![CannedResponse::json(body)]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "remember this");

    let model = responses_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "responses run should complete, got {result:?}"
    );
    let state = store
        .lock()
        .unwrap()
        .get_provider_state(&run_id)
        .expect("provider state should be readable")
        .expect("responses run should persist provider state");
    assert_eq!(state.api_kind, "openai-responses");
    assert_eq!(state.state_json, r#"{"previous_response_id":"resp_saved"}"#);
}

#[test]
fn anthropic_tool_call_round_trips_to_a_second_request() {
    // First response asks to call a tool; second response is the final answer.
    let tool_use = r#"{
        "id": "msg_tool",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "mystery_tool", "input": {"q": 1}}],
        "stop_reason": "tool_use"
    }"#;
    let final_answer = r#"{
        "id": "msg_final",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "text", "text": "All done"}],
        "stop_reason": "end_turn"
    }"#;
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(tool_use),
        CannedResponse::json(final_answer),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "use a tool");

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "tool round trip should complete, got {result:?}"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let assistant_texts: Vec<String> = final_turns
        .iter()
        .filter(|turn| turn.role == ModelTurnRole::Assistant)
        .map(|turn| turn.text_content())
        .collect();
    assert!(
        assistant_texts.iter().any(|text| text == "All done"),
        "final assistant answer should be persisted, saw {assistant_texts:?}"
    );
    assert!(
        final_turns
            .iter()
            .any(|turn| turn.role == ModelTurnRole::Tool),
        "a tool-result turn should be persisted"
    );

    // The second request must replay the tool exchange with a consistent id:
    // a real Anthropic API rejects a `tool_result` whose `tool_use_id` does not
    // match the assistant `tool_use.id` from the same conversation.
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    let second: serde_json::Value =
        serde_json::from_str(&requests[1]).expect("second request body should be JSON");
    let messages = second["messages"]
        .as_array()
        .expect("anthropic request carries a messages array");
    let tool_use_id = messages
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_use")
        .and_then(|block| block["id"].as_str())
        .expect("assistant tool_use block should be re-encoded");
    let tool_result_id = messages
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_result")
        .and_then(|block| block["tool_use_id"].as_str())
        .expect("tool_result block should be re-encoded");
    assert_eq!(
        tool_use_id, tool_result_id,
        "tool_use.id must match tool_result.tool_use_id on re-encode"
    );
}

#[test]
fn prefetched_turn_context_is_consumed_by_the_next_request_after_tools() {
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(anthropic_missing_tool_response()),
        CannedResponse::json(anthropic_final_answer_response()),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "use a tool");

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once_with_prefetcher(
        &model,
        &store,
        &session_id,
        &run_id,
        &turns,
        Arc::new(StaticPrefetcher {
            reminder: "memory recall ready\nskill discovery ready".to_string(),
        }),
    );

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "prefetch round trip should complete, got {result:?}"
    );
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        2,
        "tool use should produce a follow-up request"
    );
    assert!(
        !requests[0].contains("memory recall ready"),
        "prefetch result must not affect the already-encoded request"
    );
    assert!(
        requests[1].contains("memory recall ready")
            && requests[1].contains("skill discovery ready"),
        "prefetch result should be consumed by the next request: {}",
        requests[1]
    );
}

#[test]
fn turn_context_prefetch_overlaps_provider_response_latency() {
    let server = FakeProviderServer::start(vec![
        CannedResponse::json_delayed(
            anthropic_missing_tool_response(),
            Duration::from_millis(400),
        ),
        CannedResponse::json(anthropic_final_answer_response()),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "use a tool");

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let started = Instant::now();
    let result = run_loop_once_with_prefetcher(
        &model,
        &store,
        &session_id,
        &run_id,
        &turns,
        Arc::new(DelayedPrefetcher {
            delay: Duration::from_millis(300),
        }),
    );
    let elapsed = started.elapsed();

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "prefetch timing run should complete, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_millis(600),
        "lookup should overlap provider latency; elapsed {elapsed:?} would be close to sequential"
    );
}

#[test]
fn default_turn_context_prefetcher_recalls_relevant_session_memory() {
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(anthropic_missing_tool_response()),
        CannedResponse::json(anthropic_final_answer_response()),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    {
        let store = store.lock().unwrap();
        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turn(
                &run_id,
                message_id(1),
                ModelTurn::user_text("dragonfruit release checklist"),
            )
            .unwrap();
        store
            .append_turn(&run_id, message_id(2), ModelTurn::user_text("dragonfruit"))
            .unwrap();
    }

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "default prefetch run should complete, got {result:?}"
    );
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        2,
        "tool use should produce a follow-up request"
    );
    assert!(
        requests[1].contains("## Memory Recall")
            && requests[1].contains("dragonfruit release checklist"),
        "default prefetcher should recall relevant stored memory: {}",
        requests[1]
    );
}

#[test]
fn turn_context_prefetcher_discovers_skills_for_the_follow_up_request() {
    let skills = TestSkillRoot::new("run-loop-prefetch-skills");
    skills.write_skill(
        "ship",
        "---\nname: ship\ndescription: Ship changes safely.\n---\nbody\n",
    );
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(anthropic_missing_tool_response()),
        CannedResponse::json(anthropic_final_answer_response()),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "use a tool");

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once_with_prefetcher(
        &model,
        &store,
        &session_id,
        &run_id,
        &turns,
        Arc::new(TurnContextPrefetcher::with_skill_roots(vec![
            skills.root.clone(),
        ])),
    );

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "skill prefetch run should complete, got {result:?}"
    );
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        2,
        "tool use should produce a follow-up request"
    );
    assert!(
        requests[1].contains("## Skill Discovery")
            && requests[1].contains("ship")
            && requests[1].contains("Ship changes safely."),
        "prefetcher should disclose discovered skills in the follow-up request: {}",
        requests[1]
    );
}

#[test]
fn default_prefetcher_discovers_workspace_skills_for_the_follow_up_request() {
    let workspace = TestSkillRoot::new("run-loop-workspace-skills");
    workspace.write_workspace_skill(
        "review",
        "---\nname: review\ndescription: Review local changes.\n---\nbody\n",
    );
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(anthropic_missing_tool_response()),
        CannedResponse::json(anthropic_final_answer_response()),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "use a tool");

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let context = ToolContext::with_path_policy(workspace.policy());
    let result = run_loop_once_with_context(&model, &store, &session_id, &run_id, &turns, &context);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "workspace skill prefetch run should complete, got {result:?}"
    );
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        2,
        "tool use should produce a follow-up request"
    );
    assert!(
        requests[1].contains("## Skill Discovery")
            && requests[1].contains("review")
            && requests[1].contains("Review local changes."),
        "default prefetcher should discover workspace skills in the follow-up request: {}",
        requests[1]
    );
}

#[test]
fn anthropic_second_request_truncates_after_tool_output_without_breaking_tool_pair() {
    let tool_use = r#"{
        "id": "msg_tool",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "mystery_tool", "input": {"q": 1}}],
        "stop_reason": "tool_use"
    }"#;
    let final_answer = r#"{
        "id": "msg_final",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "text", "text": "All done"}],
        "stop_reason": "end_turn"
    }"#;
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(tool_use),
        CannedResponse::json(final_answer),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    {
        let store = store.lock().unwrap();
        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turn(
                &run_id,
                message_id(1),
                ModelTurn::user_text(format!("OLD_CONTEXT_{}", "x".repeat(90))),
            )
            .unwrap();
        store
            .append_turn(&run_id, message_id(2), ModelTurn::user_text("use a tool"))
            .unwrap();
    }

    let model = anthropic_model_with_window(server.base_url(), Some(40));
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "tool round trip should complete, got {result:?}"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    let second: serde_json::Value =
        serde_json::from_str(&requests[1]).expect("second request body should be JSON");
    let wire = second["messages"].to_string();
    assert!(
        !wire.contains("OLD_CONTEXT_"),
        "second request should drop old context after the tool exchange grows the prompt: {wire}"
    );
    let messages = second["messages"]
        .as_array()
        .expect("anthropic request carries a messages array");
    let tool_use_id = messages
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_use")
        .and_then(|block| block["id"].as_str())
        .expect("assistant tool_use block should be re-encoded");
    let tool_result_id = messages
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_result")
        .and_then(|block| block["tool_use_id"].as_str())
        .expect("tool_result block should be re-encoded");
    assert_eq!(
        tool_use_id, tool_result_id,
        "truncate must not break the re-encoded tool_use/tool_result pair"
    );
}

#[test]
fn anthropic_second_request_degrades_tool_result_when_budget_cannot_keep_pair() {
    let tool_use = r#"{
        "id": "msg_tool",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "mystery_tool", "input": {"q": 1}}],
        "stop_reason": "tool_use"
    }"#;
    let final_answer = r#"{
        "id": "msg_final",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "text", "text": "All done"}],
        "stop_reason": "end_turn"
    }"#;
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(tool_use),
        CannedResponse::json(final_answer),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "use a tool");

    let model = anthropic_model_with_window(server.base_url(), Some(1));
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "tool round trip should complete, got {result:?}"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    let second: serde_json::Value =
        serde_json::from_str(&requests[1]).expect("second request body should be JSON");
    let messages = second["messages"]
        .as_array()
        .expect("anthropic request carries a messages array");
    assert!(
        !messages
            .iter()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .any(|block| block["type"] == "tool_result"),
        "split tool result should degrade before encode: {messages:?}"
    );
    assert!(
        second["messages"].to_string().contains("unknown tool"),
        "degraded text should preserve the tool output diagnostic: {messages:?}"
    );
}

#[test]
fn openai_responses_tool_call_round_trips_to_a_second_request() {
    // The Responses encoder resolves tool-call ids through a different path than
    // Anthropic (`function_call`/`function_call_output` items keyed by
    // `call_id`), so the id-consistency guarantee needs its own coverage.
    let function_call = r#"{
        "id": "resp_tool",
        "model": "gpt-test",
        "status": "completed",
        "output": [
            {"type": "function_call", "call_id": "call_1", "name": "mystery_tool", "arguments": "{}"}
        ]
    }"#;
    let final_answer = r#"{
        "id": "resp_final",
        "model": "gpt-test",
        "status": "completed",
        "output": [{
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "All done", "annotations": []}]
        }]
    }"#;
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(function_call),
        CannedResponse::json(final_answer),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "use a tool");

    let model = responses_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "responses tool round trip should complete, got {result:?}"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    assert!(
        final_turns
            .iter()
            .filter(|turn| turn.role == ModelTurnRole::Assistant)
            .any(|turn| turn.text_content() == "All done"),
        "final assistant answer should be persisted"
    );
    assert!(
        final_turns
            .iter()
            .any(|turn| turn.role == ModelTurnRole::Tool),
        "a tool-result turn should be persisted"
    );

    // A real Responses API rejects a `function_call_output` whose `call_id` does
    // not match the `function_call` it answers.
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    let second: serde_json::Value =
        serde_json::from_str(&requests[1]).expect("second request body should be JSON");
    let input = second["input"]
        .as_array()
        .expect("responses request carries an input array");
    let call_id = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .and_then(|item| item["call_id"].as_str())
        .expect("assistant function_call should be re-encoded");
    let output_call_id = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .and_then(|item| item["call_id"].as_str())
        .expect("function_call_output should be re-encoded");
    assert_eq!(
        call_id, output_call_id,
        "function_call.call_id must match function_call_output.call_id on re-encode"
    );
}

#[test]
fn default_step_budget_finishes_with_tools_disabled_summary_turn() {
    let mut responses: Vec<CannedResponse> = (1..80)
        .map(|step| CannedResponse::json(&anthropic_tool_use_response(step)))
        .collect();
    responses.push(CannedResponse::json(
        r#"{
            "id": "msg_final",
            "model": "claude-test",
            "role": "assistant",
            "content": [{"type": "text", "text": "Budget summary"}],
            "stop_reason": "end_turn"
        }"#,
    ));
    let server = FakeProviderServer::start(responses);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "keep using tools");

    let registry = registry_with_read_tool();
    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result =
        run_loop_once_with_registry(&model, &store, &session_id, &run_id, &turns, &registry);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "budgeted run should complete with a final text turn, got {result:?}"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 80, "default budget should stop at step 80");
    let first_request: serde_json::Value =
        serde_json::from_str(&requests[0]).expect("first request body should be JSON");
    assert!(
        first_request.get("tools").is_some(),
        "tool definitions should be available before the final step"
    );
    let final_request: serde_json::Value =
        serde_json::from_str(&requests[79]).expect("final request body should be JSON");
    assert!(
        final_request.get("tools").is_none(),
        "final-step request should disable tools"
    );
    let final_messages = final_request["messages"]
        .as_array()
        .expect("final request should carry messages");
    assert!(
        final_messages.iter().any(|message| {
            message["role"] == "assistant"
                && message["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|block| {
                        block["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("Tools are now disabled"))
                    })
        }),
        "final request should include the synthetic text-only summary instruction"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    assert!(
        final_turns.iter().any(|turn| {
            turn.role == ModelTurnRole::Assistant
                && matches!(
                    turn.parts.as_slice(),
                    [TurnPart::Text {
                        synthetic: Some(true),
                        ..
                    }]
                )
                && turn.text_content().contains("Tools are now disabled")
        }),
        "synthetic final-step instruction should be persisted for replay"
    );
    assert!(
        final_turns
            .iter()
            .filter(|turn| turn.role == ModelTurnRole::Assistant)
            .any(|turn| turn.text_content() == "Budget summary"),
        "provider's final text-only summary should be persisted"
    );
}

#[test]
fn final_step_does_not_dispatch_provider_tool_calls() {
    let mut responses: Vec<CannedResponse> = (1..80)
        .map(|step| CannedResponse::json(&anthropic_tool_use_response(step)))
        .collect();
    responses.push(CannedResponse::json(&anthropic_text_and_tool_use_response(
        "Budget summary",
        80,
        "after-budget.txt",
    )));
    let server = FakeProviderServer::start(responses);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "keep using tools");

    let registry = registry_with_read_tool();
    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result =
        run_loop_once_with_registry(&model, &store, &session_id, &run_id, &turns, &registry);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "final-step tool calls should not push the loop past the step budget, got {result:?}"
    );
    assert_eq!(
        server.requests().len(),
        80,
        "run should stop at the final step"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let final_assistant = final_turns
        .iter()
        .rev()
        .find(|turn| turn.role == ModelTurnRole::Assistant)
        .expect("final assistant turn should be persisted");
    assert_eq!(final_assistant.text_content(), "Budget summary");
    assert!(
        final_assistant.tool_calls().is_empty(),
        "final assistant turn should stay text-only even if the provider returned a tool call"
    );
    assert!(
        final_turns.iter().all(|turn| {
            !matches!(
                turn.parts.as_slice(),
                [TurnPart::ToolResult { tool_call_id, .. }] if tool_call_id == "toolu_80"
            )
        }),
        "final-step tool call should not execute or persist a tool result"
    );
}

#[test]
fn third_consecutive_identical_tool_call_returns_doom_loop_tool_result() {
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(&anthropic_tool_use_response_with_path(1, "repeat.txt")),
        CannedResponse::json(&anthropic_tool_use_response_with_path(2, "repeat.txt")),
        CannedResponse::json(&anthropic_tool_use_response_with_path(3, "repeat.txt")),
        CannedResponse::json(
            r#"{
                "id": "msg_final",
                "model": "claude-test",
                "role": "assistant",
                "content": [{"type": "text", "text": "Changed approach"}],
                "stop_reason": "end_turn"
            }"#,
        ),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(
        &store,
        &session_id,
        &run_id,
        "read the same file repeatedly",
    );

    let registry = registry_with_read_tool();
    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result =
        run_loop_once_with_registry(&model, &store, &session_id, &run_id, &turns, &registry);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "doom-loop guarded run should complete, got {result:?}"
    );

    let requests = server.requests();
    assert_eq!(
        requests.len(),
        4,
        "the synthetic doom-loop result should be sent to the next model turn"
    );
    assert!(
        requests[3].contains(DOOM_LOOP_READ_MESSAGE),
        "next provider request should include the synthetic doom-loop tool result"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    assert!(
        final_turns.iter().any(|turn| {
            turn.role == ModelTurnRole::Tool
                && matches!(
                    turn.parts.as_slice(),
                    [TurnPart::ToolResult { content, .. }]
                        if content == DOOM_LOOP_READ_MESSAGE
                )
        }),
        "third identical call should persist the synthetic doom-loop tool result"
    );
}

#[test]
fn batched_doom_loop_result_keeps_original_tool_result_order() {
    let server = FakeProviderServer::start(vec![
        CannedResponse::json(&anthropic_three_identical_tool_uses_response()),
        CannedResponse::json(
            r#"{
                "id": "msg_final",
                "model": "claude-test",
                "role": "assistant",
                "content": [{"type": "text", "text": "Changed approach"}],
                "stop_reason": "end_turn"
            }"#,
        ),
    ]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "try repeated tool calls");

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let (result, events) =
        run_loop_once_collecting_events(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "doom-loop guarded batch should complete, got {result:?}"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "tool batch should be returned once");
    let first_error_index = requests[1]
        .find("unknown tool `repeat_tool`")
        .expect("first executable tool result should be replayed");
    let doom_loop_index = requests[1]
        .find(DOOM_LOOP_REPEAT_TOOL_MESSAGE)
        .expect("synthetic doom-loop result should be replayed");
    assert!(
        first_error_index < doom_loop_index,
        "synthetic doom-loop result should keep the original third-call position"
    );

    let failed_messages = tool_call_failed_messages(&events);
    assert_eq!(
        failed_messages,
        vec![
            "unknown tool `repeat_tool`".to_string(),
            "unknown tool `repeat_tool`".to_string(),
            DOOM_LOOP_REPEAT_TOOL_MESSAGE.to_string(),
        ],
        "streamed failure events should follow the original tool-call order"
    );
}

#[test]
fn cancelling_a_run_aborts_the_in_flight_dialect_request() {
    // A server that accepts the connection but never answers: without
    // cancellation propagation the run would block until the request timeout.
    let server = HangingProviderServer::start();

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    seed_user_turn(&store, &session_id, &run_id, "say hello");

    let token = OpenAiCompletionsCancellationToken::new();
    let token_for_canceller = token.clone();
    // Cancel shortly after the request is in flight (well under the 30s client
    // request timeout, so reaching the timeout would indicate a regression).
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        token_for_canceller.cancel();
    });

    let model = anthropic_model(server.base_url());
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let started = Instant::now();
    let result = run_loop_with_token(&model, &store, &session_id, &run_id, &turns, token);
    let elapsed = started.elapsed();
    canceller.join().unwrap();

    assert!(
        matches!(result, RunLoopResult::Cancelled),
        "cancelling mid-request should yield Cancelled, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "cancellation should abort promptly, took {elapsed:?}"
    );
}

#[test]
fn chat_completions_degrades_orphaned_tool_result_before_encode() {
    let server = FakeProviderServer::start(vec![CannedResponse::sse("All set")]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    {
        let store = store.lock().unwrap();
        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turn(&run_id, message_id(1), ModelTurn::user_text("continue"))
            .unwrap();
        store
            .append_turn(
                &run_id,
                message_id(2),
                ModelTurn::tool_result("missing-call", "orphaned output"),
            )
            .unwrap();
        store
            .append_turn(&run_id, message_id(3), ModelTurn::user_text("finish"))
            .unwrap();
    }

    let model = chat_model(server.base_url(), None);
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "chat run should complete, got {result:?}"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "expected one provider request");
    let body: serde_json::Value =
        serde_json::from_str(&requests[0]).expect("request body should be JSON");
    let messages = body["messages"]
        .as_array()
        .expect("chat completions request carries messages");
    assert!(
        !messages.iter().any(|message| message["role"] == "tool"),
        "orphaned tool result should not be sent as a tool message: {messages:?}"
    );
    assert!(
        messages.iter().any(|message| {
            message["role"] == "assistant"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("orphaned output"))
        }),
        "orphaned tool result should be preserved as synthetic assistant text: {messages:?}"
    );
}

#[test]
fn chat_completions_truncates_history_to_model_context_budget_before_encode() {
    let server = FakeProviderServer::start(vec![CannedResponse::sse("Done")]);

    let store = Arc::new(Mutex::new(SessionStore::default()));
    let session_id = session_id();
    let run_id = run_id(1);
    let bulky_old_turn = format!("OLD_CONTEXT_{}", "x".repeat(600));
    {
        let store = store.lock().unwrap();
        store.create_session(session_id.clone()).unwrap();
        store.start_run(&session_id, run_id.clone()).unwrap();
        store
            .append_turn(
                &run_id,
                message_id(1),
                ModelTurn::user_text(&bulky_old_turn),
            )
            .unwrap();
        store
            .append_turn(
                &run_id,
                message_id(2),
                ModelTurn::user_text("latest prompt"),
            )
            .unwrap();
    }

    let model = chat_model(server.base_url(), Some(32));
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_loop_once(&model, &store, &session_id, &run_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "chat run should complete, got {result:?}"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "expected one provider request");
    let body: serde_json::Value =
        serde_json::from_str(&requests[0]).expect("request body should be JSON");
    let wire = body["messages"].to_string();
    assert!(
        !wire.contains("OLD_CONTEXT_"),
        "oversized old context should be dropped before encode: {wire}"
    );
    assert!(
        wire.contains("latest prompt"),
        "latest prompt must survive truncation: {wire}"
    );
}

fn run_loop_once(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    turns: &[ModelTurn],
) -> RunLoopResult {
    let registry = ToolRegistry::default();
    run_loop_once_with_registry(model, store, session_id, run_id, turns, &registry)
}

fn run_loop_once_with_registry(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    turns: &[ModelTurn],
    registry: &ToolRegistry,
) -> RunLoopResult {
    run_loop_with_token_and_registry(
        model,
        store,
        session_id,
        run_id,
        turns,
        registry,
        OpenAiCompletionsCancellationToken::new(),
    )
}

fn run_loop_once_collecting_events(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    turns: &[ModelTurn],
) -> (RunLoopResult, Vec<HarnessEventEnvelope>) {
    let registry = ToolRegistry::default();
    let context = ToolContext::default();
    let mut ids = TestIds::default();
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());
    let mut events = Vec::new();
    let result = run_loop.run(
        model,
        RunLoopRequest {
            session_id,
            run_id,
            message_id: &message_id(0),
            turns,
            tool_registry: &registry,
            tool_preset: ToolPreset::Coding,
            tool_context: &context,
            session_store: Some(store),
            pending_confirmations: None,
            compaction_model_resolver: None,
            cancellation_token: OpenAiCompletionsCancellationToken::new(),
        },
        &mut ids,
        |envelopes| events.extend(envelopes),
    );

    (result, events)
}

fn run_loop_with_token(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    turns: &[ModelTurn],
    cancellation_token: OpenAiCompletionsCancellationToken,
) -> RunLoopResult {
    let registry = ToolRegistry::default();
    run_loop_with_token_and_registry(
        model,
        store,
        session_id,
        run_id,
        turns,
        &registry,
        cancellation_token,
    )
}

fn run_loop_with_token_and_registry(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    turns: &[ModelTurn],
    registry: &ToolRegistry,
    cancellation_token: OpenAiCompletionsCancellationToken,
) -> RunLoopResult {
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());
    let context = ToolContext::default();
    let mut ids = TestIds::default();

    run_loop.run(
        model,
        RunLoopRequest {
            session_id,
            run_id,
            message_id: &message_id(0),
            turns,
            tool_registry: registry,
            tool_preset: ToolPreset::Coding,
            tool_context: &context,
            session_store: Some(store),
            pending_confirmations: None,
            compaction_model_resolver: None,
            cancellation_token,
        },
        &mut ids,
        |_events| {},
    )
}

fn run_loop_once_with_context(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    turns: &[ModelTurn],
    context: &ToolContext,
) -> RunLoopResult {
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());
    let registry = ToolRegistry::default();
    let mut ids = TestIds::default();

    run_loop.run(
        model,
        RunLoopRequest {
            session_id,
            run_id,
            message_id: &message_id(0),
            turns,
            tool_registry: &registry,
            tool_preset: ToolPreset::Coding,
            tool_context: context,
            session_store: Some(store),
            pending_confirmations: None,
            cancellation_token: OpenAiCompletionsCancellationToken::new(),
        },
        &mut ids,
        |_events| {},
    )
}

fn run_loop_once_with_prefetcher(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    turns: &[ModelTurn],
    lookup_prefetcher: Arc<dyn TurnLookupPrefetcher>,
) -> RunLoopResult {
    let run_loop =
        RunLoop::with_client_and_prefetcher(OpenAiCompletionsClient::new(), lookup_prefetcher);
    let registry = ToolRegistry::default();
    let context = ToolContext::default();
    let mut ids = TestIds::default();

    run_loop.run(
        model,
        RunLoopRequest {
            session_id,
            run_id,
            message_id: &message_id(0),
            turns,
            tool_registry: &registry,
            tool_preset: ToolPreset::Coding,
            tool_context: &context,
            session_store: Some(store),
            pending_confirmations: None,
            cancellation_token: OpenAiCompletionsCancellationToken::new(),
        },
        &mut ids,
        |_events| {},
    )
}

fn registry_with_read_tool() -> ToolRegistry {
    let mut registry = ToolRegistry::default();
    read::register(&mut registry).unwrap();
    registry
}

fn tool_call_failed_messages(events: &[HarnessEventEnvelope]) -> Vec<String> {
    events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            HarnessEvent::ToolCallFailed { error_message, .. } => Some(error_message.clone()),
            _ => None,
        })
        .collect()
}

fn anthropic_tool_use_response(step: usize) -> String {
    anthropic_tool_use_response_with_path(step, &format!("file-{step}.txt"))
}

fn anthropic_tool_use_response_with_path(step: usize, path: &str) -> String {
    format!(
        r#"{{
            "id": "msg_tool_{step}",
            "model": "claude-test",
            "role": "assistant",
            "content": [{{
                "type": "tool_use",
                "id": "toolu_{step}",
                "name": "read",
                "input": {{"path": "{path}"}}
            }}],
            "stop_reason": "tool_use"
        }}"#
    )
}

fn anthropic_text_and_tool_use_response(text: &str, step: usize, path: &str) -> String {
    format!(
        r#"{{
            "id": "msg_tool_{step}",
            "model": "claude-test",
            "role": "assistant",
            "content": [
                {{"type": "text", "text": "{text}"}},
                {{
                    "type": "tool_use",
                    "id": "toolu_{step}",
                    "name": "read",
                    "input": {{"path": "{path}"}}
                }}
            ],
            "stop_reason": "tool_use"
        }}"#
    )
}

fn anthropic_three_identical_tool_uses_response() -> String {
    r#"{
        "id": "msg_repeat_batch",
        "model": "claude-test",
        "role": "assistant",
        "content": [
            {"type": "tool_use", "id": "toolu_repeat_1", "name": "repeat_tool", "input": {"path": "repeat.txt"}},
            {"type": "tool_use", "id": "toolu_repeat_2", "name": "repeat_tool", "input": {"path": "repeat.txt"}},
            {"type": "tool_use", "id": "toolu_repeat_3", "name": "repeat_tool", "input": {"path": "repeat.txt"}}
        ],
        "stop_reason": "tool_use"
    }"#
    .to_string()
}

fn seed_user_turn(
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    text: &str,
) {
    let store = store.lock().unwrap();
    store.create_session(session_id.clone()).unwrap();
    store.start_run(session_id, run_id.clone()).unwrap();
    store
        .append_turn(run_id, message_id(1), ModelTurn::user_text(text))
        .unwrap();
}

fn anthropic_model(base_url: &str) -> ResolvedModelConfig {
    resolved_model(base_url, ApiKind::AnthropicMessages, "claude-test")
}

fn anthropic_model_with_window(base_url: &str, context_window: Option<u32>) -> ResolvedModelConfig {
    resolved_model_with_window(
        base_url,
        ApiKind::AnthropicMessages,
        "claude-test",
        context_window,
    )
}

fn responses_model(base_url: &str) -> ResolvedModelConfig {
    resolved_model(base_url, ApiKind::OpenAiResponses, "gpt-test")
}

fn anthropic_missing_tool_response() -> &'static str {
    r#"{
        "id": "msg_tool",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "missing_tool", "input": {"q": 1}}],
        "stop_reason": "tool_use"
    }"#
}

fn anthropic_final_answer_response() -> &'static str {
    r#"{
        "id": "msg_final",
        "model": "claude-test",
        "role": "assistant",
        "content": [{"type": "text", "text": "All done"}],
        "stop_reason": "end_turn"
    }"#
}

fn chat_model(base_url: &str, context_window: Option<u32>) -> ResolvedModelConfig {
    resolved_model_with_window(
        base_url,
        ApiKind::OpenAiCompletions,
        "gpt-chat-test",
        context_window,
    )
}

fn resolved_model(base_url: &str, api: ApiKind, model_id: &str) -> ResolvedModelConfig {
    resolved_model_with_window(base_url, api, model_id, None)
}

fn resolved_model_with_window(
    base_url: &str,
    api: ApiKind,
    model_id: &str,
    context_window: Option<u32>,
) -> ResolvedModelConfig {
    let mut providers = BTreeMap::new();
    providers.insert(
        "test-provider".to_string(),
        ProviderConfig {
            name: None,
            api,
            base_url: base_url.to_string(),
            api_key: ApiKeyConfig::Inline {
                inline: "test-secret".to_string(),
            },
            models: vec![ModelConfig {
                id: model_id.to_string(),
                name: None,
                api: None,
                base_url: None,
                reasoning: false,
                input: Vec::new(),
                context_window,
                max_tokens: Some(256),
                compat: Default::default(),
            }],
            compat: Default::default(),
        },
    );

    ModelResolver::new(ModelSettings {
        default_model: Some(ModelRef {
            provider: "test-provider".to_string(),
            model: model_id.to_string(),
        }),
        providers,
        ..ModelSettings::default()
    })
    .resolve_default()
    .unwrap()
}

#[derive(Debug)]
struct StaticPrefetcher {
    reminder: String,
}

impl TurnLookupPrefetcher for StaticPrefetcher {
    fn start(&self, _request: TurnLookupPrefetchRequest<'_>) -> Box<dyn PendingTurnLookupResults> {
        Box::new(StaticPrefetch {
            reminder: self.reminder.clone(),
        })
    }
}

#[derive(Debug)]
struct StaticPrefetch {
    reminder: String,
}

impl PendingTurnLookupResults for StaticPrefetch {
    fn resolve(self: Box<Self>) -> PrefetchedTurnContext {
        PrefetchedTurnContext::from_system_context(self.reminder)
    }
}

#[derive(Debug)]
struct DelayedPrefetcher {
    delay: Duration,
}

impl TurnLookupPrefetcher for DelayedPrefetcher {
    fn start(&self, _request: TurnLookupPrefetchRequest<'_>) -> Box<dyn PendingTurnLookupResults> {
        let delay = self.delay;
        Box::new(DelayedPrefetch {
            handle: thread::spawn(move || thread::sleep(delay)),
        })
    }
}

#[derive(Debug)]
struct DelayedPrefetch {
    handle: JoinHandle<()>,
}

impl PendingTurnLookupResults for DelayedPrefetch {
    fn resolve(self: Box<Self>) -> PrefetchedTurnContext {
        self.handle.join().expect("prefetch thread should finish");
        PrefetchedTurnContext::default()
    }
}

struct TestSkillRoot {
    root: PathBuf,
}

impl TestSkillRoot {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("nav-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("skill root should be created");
        Self { root }
    }

    fn write_skill(&self, name: &str, contents: &str) {
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).expect("skill directory should be created");
        fs::write(dir.join("SKILL.md"), contents).expect("skill file should be written");
    }

    fn write_workspace_skill(&self, name: &str, contents: &str) {
        let dir = self.root.join(".nav").join("skills").join(name);
        fs::create_dir_all(&dir).expect("workspace skill directory should be created");
        fs::write(dir.join("SKILL.md"), contents).expect("workspace skill file should be written");
    }

    fn policy(&self) -> WorkspacePathPolicy {
        WorkspacePathPolicy::new(&self.root, &self.root).expect("workspace policy should build")
    }
}

impl Drop for TestSkillRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone)]
struct CannedResponse {
    status: u16,
    content_type: &'static str,
    body: String,
    delay: Duration,
}

impl CannedResponse {
    fn json(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.to_string(),
            delay: Duration::ZERO,
        }
    }

    fn json_delayed(body: &str, delay: Duration) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.to_string(),
            delay,
        }
    }

    fn sse(text: &str) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            body: format!(
                "data: {{\"id\":\"chatcmpl_test\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"chatcmpl_test\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
            ),
            delay: Duration::ZERO,
        }
    }
}

/// A fake provider that answers a fixed queue of canned responses, one per
/// incoming connection.
struct FakeProviderServer {
    base_url: String,
    handle: Option<JoinHandle<()>>,
    handled: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeProviderServer {
    fn start(responses: Vec<CannedResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake server should bind");
        listener
            .set_nonblocking(true)
            .expect("fake server should set non-blocking");
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let handled = Arc::new(AtomicUsize::new(0));
        let handled_in_thread = Arc::clone(&handled);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_in_thread = Arc::clone(&requests);

        let handle = thread::spawn(move || {
            for response in responses {
                let Some(mut stream) = accept_before(&listener, Duration::from_secs(10)) else {
                    return;
                };
                let body = drain_http_request(&mut stream);
                requests_in_thread.lock().unwrap().push(body);
                if response.delay > Duration::ZERO {
                    thread::sleep(response.delay);
                }

                let header = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    reason_phrase(response.status),
                    response.content_type,
                    response.body.len(),
                );
                stream.write_all(header.as_bytes()).expect("header writes");
                stream
                    .write_all(response.body.as_bytes())
                    .expect("body writes");
                stream.flush().expect("response flushes");
                handled_in_thread.fetch_add(1, Ordering::SeqCst);
            }
        });

        Self {
            base_url,
            handle: Some(handle),
            handled,
            requests,
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Captured request bodies, in the order the server answered them.
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeProviderServer {
    fn drop(&mut self) {
        let _ = self.handled.load(Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A fake provider that accepts one connection and then holds it open without
/// ever sending a response, so a request only ends via cancellation or timeout.
struct HangingProviderServer {
    base_url: String,
    handle: Option<JoinHandle<()>>,
}

impl HangingProviderServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("hanging server should bind");
        listener
            .set_nonblocking(true)
            .expect("hanging server should set non-blocking");
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());

        let handle = thread::spawn(move || {
            if let Some(mut stream) = accept_before(&listener, Duration::from_secs(10)) {
                drain_http_request(&mut stream);
                // Hold the connection without responding, long enough to outlast
                // the ~200ms cancellation but bounded so a regression (waiting on
                // the response) fails fast instead of hanging the test.
                thread::sleep(Duration::from_millis(1500));
            }
        });

        Self {
            base_url,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for HangingProviderServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn accept_before(listener: &TcpListener, timeout: Duration) -> Option<TcpStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

fn drain_http_request(stream: &mut TcpStream) -> String {
    stream
        .set_nonblocking(false)
        .expect("blocking mode for read");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return String::new();
        }
        let trimmed = line.trim_end();
        if let Some(value) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
        if trimmed.is_empty() {
            break;
        }
    }
    let mut body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Unknown",
    }
}

#[derive(Default)]
struct TestIds {
    counter: u64,
}

impl TestIds {
    fn next_uuid(&mut self) -> String {
        self.counter += 1;
        format!("019f2f6f-f178-7a72-9f28-{:012x}", self.counter)
    }
}

impl HarnessEventIdSource for TestIds {
    fn next_event_id(&mut self) -> EventId {
        EventId::try_new(self.next_uuid()).unwrap()
    }

    fn next_tool_call_id(&mut self) -> ToolCallId {
        ToolCallId::try_new(self.next_uuid()).unwrap()
    }

    fn next_approval_id(&mut self) -> ApprovalId {
        ApprovalId::try_new(self.next_uuid()).unwrap()
    }
}

fn session_id() -> SessionId {
    SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap()
}

fn run_id(suffix: u64) -> RunId {
    RunId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012}")).unwrap()
}

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f28-1{suffix:011}")).unwrap()
}
