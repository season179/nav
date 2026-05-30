use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nav_harness::agents::{RunLoop, RunLoopRequest, RunLoopResult};
use nav_harness::compaction::breaker::TRANSIENT_COOLDOWN_WARNING;
use nav_harness::compaction::overflow::OVERFLOW_CONTINUATION_TEXT;
use nav_harness::events::HarnessEventIdSource;
use nav_harness::models::{
    ApiKeyConfig, ApiKind, ModelConfig, ModelRef, ModelResolver, ModelSettings,
    OpenAiCompletionsCancellationToken, OpenAiCompletionsClient, ProviderConfig,
    ResolvedModelConfig,
};
use nav_harness::sessions::{ModelTurn, ModelTurnRole, SessionStore, TurnPart};
use nav_harness::tools::{ToolContext, ToolPreset, ToolRegistry};
use nav_types::{ApprovalId, EventId, MessageId, RunId, SessionId, ToolCallId};

#[test]
fn overflow_continuation_appends_single_synthetic_user_turn() {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id(1);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    store
        .append_turn(&run_id, message_id(0), ModelTurn::user_text("do the thing"))
        .unwrap();
    store
        .append_turn(
            &run_id,
            message_id(1),
            ModelTurn::assistant_text("working on it"),
        )
        .unwrap();

    store
        .append_overflow_continuation(&session_id, &run_id)
        .unwrap();

    let turns = store.try_turns(&session_id).unwrap();
    let last = turns
        .last()
        .expect("a turn should exist after continuation");
    assert_eq!(last.role, ModelTurnRole::User);
    assert_eq!(last.text_content(), OVERFLOW_CONTINUATION_TEXT);
    assert_eq!(continuation_count(&turns), 1);
}

#[test]
fn overflow_continuation_is_not_duplicated_on_repeated_recovery() {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id(1);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    store
        .append_turn(&run_id, message_id(0), ModelTurn::user_text("do the thing"))
        .unwrap();

    store
        .append_overflow_continuation(&session_id, &run_id)
        .unwrap();
    store
        .append_overflow_continuation(&session_id, &run_id)
        .unwrap();

    let turns = store.try_turns(&session_id).unwrap();
    assert_eq!(continuation_count(&turns), 1);
    assert_eq!(
        turns.last().map(ModelTurn::text_content),
        Some(OVERFLOW_CONTINUATION_TEXT.to_string())
    );
}

#[test]
fn overflow_replay_turn_reissues_original_text_once() {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id(1);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    store
        .append_turn(
            &run_id,
            message_id(0),
            ModelTurn::user_text("finish the exact task"),
        )
        .unwrap();

    store
        .append_overflow_replay_turn(&session_id, &run_id, "finish the exact task")
        .unwrap();
    store
        .append_overflow_replay_turn(&session_id, &run_id, "finish the exact task")
        .unwrap();

    let turns = store.try_turns(&session_id).unwrap();
    assert_eq!(
        synthetic_user_text_count(&turns, "finish the exact task"),
        1
    );
    assert_eq!(
        turns.last().map(ModelTurn::text_content),
        Some("finish the exact task".to_string())
    );
}

#[test]
fn overflow_error_triggers_compaction_and_completes_on_retry() {
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_summary(&valid_summary("Finish the overflow handler.")),
        canned_stream_completion("Resuming after compaction."),
    ]);
    let store = oversize_session();
    let session_id = session_id();
    let run_id = run_id(1);
    let trigger_id = message_id(99);

    let model = resolved_model(server.base_url());
    let store = Arc::new(Mutex::new(store));
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();

    let result = run_overflow_loop(&model, &store, &session_id, &run_id, &trigger_id, &turns);

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "overflow recovery should complete on retry, got {result:?}"
    );
    assert_eq!(server.handled(), 3);

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    assert_eq!(
        synthetic_user_text_count(&final_turns, "please finish the overflow handler"),
        1,
        "the original user turn should be re-issued once"
    );
    assert_eq!(
        last_user_turn(&final_turns).map(ModelTurn::text_content),
        Some("please finish the overflow handler".to_string()),
        "overflow recovery should replay the original triggering user turn verbatim"
    );
    assert!(
        final_turns
            .iter()
            .any(|turn| turn.text_content().contains("Finish the overflow handler")),
        "the compaction summary should remain in the replay window"
    );

    let request_bodies = server.request_bodies();
    assert_eq!(request_bodies.len(), 3);
    assert!(
        request_bodies[1].contains("\"model\":\"overflow-model\""),
        "without an override, the summary request should use the session model:\n{}",
        request_bodies[1]
    );
}

#[test]
fn overflow_compaction_uses_model_override_when_configured() {
    let session_server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_stream_completion("Done."),
    ]);
    let summary_server =
        FakeProviderServer::start(vec![canned_summary(&valid_summary("Use cheaper summary."))]);
    let oversized_result = "x".repeat(6_000);
    let store = oversize_session_with_strippable_summary_payload(&oversized_result);
    let session_id = session_id();
    let run_id = run_id(1);
    let trigger_id = message_id(99);

    let session_model = resolved_model_with_id(session_server.base_url(), "session-model");
    let summary_model_resolver =
        compaction_override_resolver(summary_server.base_url(), "summary-model");
    let store = Arc::new(Mutex::new(store));
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();

    let result = run_overflow_loop_with_override(
        &session_model,
        Some(&summary_model_resolver),
        &store,
        &session_id,
        &run_id,
        &trigger_id,
        &turns,
    );

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "overflow recovery should complete with an override model, got {result:?}"
    );
    assert_eq!(
        session_server.handled(),
        2,
        "session model should only receive the failed request and retry"
    );
    assert_eq!(
        summary_server.handled(),
        1,
        "override model should receive the compaction summary request"
    );

    let summary_bodies = summary_server.request_bodies();
    assert_eq!(summary_bodies.len(), 1);
    assert!(
        summary_bodies[0].contains("\"model\":\"summary-model\""),
        "summary request should target override model but was:\n{}",
        summary_bodies[0]
    );
    assert!(
        !summary_bodies[0].contains("[image elided]"),
        "override summary request should strip image placeholders:\n{}",
        summary_bodies[0]
    );
    assert!(
        !summary_bodies[0].contains(&oversized_result),
        "override summary request should truncate oversized tool results"
    );
    assert!(
        !summary_bodies[0].contains("\"tools\""),
        "override summary request should not carry tool definitions:\n{}",
        summary_bodies[0]
    );
}

#[test]
fn overflow_recovery_is_bounded_and_fails_without_looping() {
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_summary(&valid_summary("Still too large.")),
        canned_context_limit(),
    ]);
    let store = oversize_session();
    let session_id = session_id();
    let run_id = run_id(1);
    let trigger_id = message_id(99);

    let model = resolved_model(server.base_url());
    let store = Arc::new(Mutex::new(store));
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();

    let result = run_overflow_loop(&model, &store, &session_id, &run_id, &trigger_id, &turns);

    let RunLoopResult::Failed(nav_harness::models::OpenAiCompletionsError::ContextOverflow {
        message,
    }) = result
    else {
        panic!("a second consecutive overflow should surface ContextOverflowError, got {result:?}");
    };
    assert!(
        message.contains("compacted summary and retained tail still exceed the context window"),
        "terminal overflow should explain Stage-5 fallback, got {message:?}"
    );
    assert_eq!(
        server.handled(),
        3,
        "recovery should be attempted exactly once before failing"
    );

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    assert!(
        !final_turns
            .iter()
            .any(|turn| turn.text_content().contains("Still too large.")),
        "Stage-5 fallback should drop the unhelpful summary"
    );
    assert_eq!(
        synthetic_user_text_count(&final_turns, "please finish the overflow handler"),
        0,
        "Stage-5 fallback should drop the synthetic replay turn"
    );
}

#[test]
fn malformed_overflow_summary_is_rejected_without_committing() {
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_summary("not a real summary"),
    ]);
    let store = oversize_session();
    let session_id = session_id();
    let run_id = run_id(1);
    let trigger_id = message_id(99);

    let model = resolved_model(server.base_url());
    let store = Arc::new(Mutex::new(store));
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();

    let result = run_overflow_loop(&model, &store, &session_id, &run_id, &trigger_id, &turns);

    assert_malformed_failure(result, "malformed summary recovery");
    assert_eq!(server.handled(), 2);

    let final_turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    assert!(
        !final_turns
            .iter()
            .any(|turn| turn.text_content().contains("not a real summary")),
        "invalid summary must not be inserted into replay"
    );
    assert_eq!(
        synthetic_user_text_count(&final_turns, "please finish the overflow handler"),
        0,
        "replay turn should not be appended when summary validation fails"
    );
}

#[test]
fn repeated_malformed_summaries_trip_failure_breaker() {
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_summary("not a real summary 1"),
        canned_context_limit(),
        canned_summary("not a real summary 2"),
        canned_context_limit(),
        canned_summary("not a real summary 3"),
        canned_context_limit(),
    ]);
    let store = Arc::new(Mutex::new(oversize_session()));
    let session_id = session_id();
    let model = resolved_model(server.base_url());
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());

    for attempt in 1..=3 {
        let run_id = run_id(attempt + 1);
        let trigger_id = message_id(100 + attempt);
        append_user_turn_for_run(
            &store,
            &session_id,
            &run_id,
            trigger_id.clone(),
            &format!("overflow request {attempt}"),
        );
        let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
        let result = run_overflow_loop_with(
            &run_loop,
            &model,
            &store,
            &session_id,
            &run_id,
            &trigger_id,
            &turns,
        );

        assert_malformed_failure(result, &format!("malformed summary attempt {attempt}"));
    }

    let blocked_run_id = run_id(5);
    let blocked_trigger_id = message_id(104);
    append_user_turn_for_run(
        &store,
        &session_id,
        &blocked_run_id,
        blocked_trigger_id.clone(),
        "blocked overflow request",
    );
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_overflow_loop_with(
        &run_loop,
        &model,
        &store,
        &session_id,
        &blocked_run_id,
        &blocked_trigger_id,
        &turns,
    );

    let RunLoopResult::Failed(nav_harness::models::OpenAiCompletionsError::MalformedResponse {
        message,
    }) = result
    else {
        panic!("breaker should fail without attempting another summary, got {result:?}");
    };
    assert!(
        message.contains("Auto-compaction disabled"),
        "breaker warning should be surfaced, got {message:?}"
    );
    assert_eq!(
        server.handled(),
        7,
        "fourth overflow should stop after the provider context-limit response"
    );
}

#[test]
fn repeated_low_savings_compactions_trip_anti_thrashing_guard() {
    let low_savings_summary = long_valid_summary();
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_summary(&low_savings_summary),
        canned_stream_completion("first retry completed"),
        canned_context_limit(),
        canned_summary(&low_savings_summary),
        canned_stream_completion("second retry completed"),
        canned_context_limit(),
    ]);
    let store = Arc::new(Mutex::new(oversize_session()));
    let session_id = session_id();
    let model = resolved_model(server.base_url());
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());

    for attempt in 1..=2 {
        let run_id = run_id(attempt + 1);
        let trigger_id = message_id(120 + attempt);
        append_user_turn_for_run(
            &store,
            &session_id,
            &run_id,
            trigger_id.clone(),
            &format!("low savings request {attempt}"),
        );
        let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
        let result = run_overflow_loop_with(
            &run_loop,
            &model,
            &store,
            &session_id,
            &run_id,
            &trigger_id,
            &turns,
        );

        assert!(
            matches!(result, RunLoopResult::Completed(_)),
            "low-savings recovery {attempt} should complete, got {result:?}"
        );
    }

    let blocked_run_id = run_id(7);
    let blocked_trigger_id = message_id(127);
    append_user_turn_for_run(
        &store,
        &session_id,
        &blocked_run_id,
        blocked_trigger_id.clone(),
        "blocked by anti-thrashing",
    );
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_overflow_loop_with(
        &run_loop,
        &model,
        &store,
        &session_id,
        &blocked_run_id,
        &blocked_trigger_id,
        &turns,
    );

    let RunLoopResult::Failed(nav_harness::models::OpenAiCompletionsError::MalformedResponse {
        message,
    }) = result
    else {
        panic!("anti-thrashing guard should fail before another summary, got {result:?}");
    };
    assert!(
        message.contains("Automatic compaction paused"),
        "anti-thrashing warning should be surfaced, got {message:?}"
    );
    assert_eq!(
        server.handled(),
        7,
        "third overflow should stop after the provider context-limit response"
    );
}

#[test]
fn reset_compaction_breakers_clears_anti_thrashing_guard() {
    let low_savings_summary = long_valid_summary();
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_summary(&low_savings_summary),
        canned_stream_completion("first retry completed"),
        canned_context_limit(),
        canned_summary(&low_savings_summary),
        canned_stream_completion("second retry completed"),
        canned_context_limit(),
        canned_summary(&valid_summary("Recovered after anti-thrashing reset.")),
        canned_stream_completion("reset recovery completed"),
    ]);
    let store = Arc::new(Mutex::new(oversize_session()));
    let session_id = session_id();
    let model = resolved_model(server.base_url());
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());

    for attempt in 1..=2 {
        let run_id = run_id(attempt + 1);
        let trigger_id = message_id(160 + attempt);
        append_user_turn_for_run(
            &store,
            &session_id,
            &run_id,
            trigger_id.clone(),
            &format!("low savings before reset {attempt}"),
        );
        let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
        let result = run_overflow_loop_with(
            &run_loop,
            &model,
            &store,
            &session_id,
            &run_id,
            &trigger_id,
            &turns,
        );

        assert!(
            matches!(result, RunLoopResult::Completed(_)),
            "low-savings recovery {attempt} should complete, got {result:?}"
        );
    }

    run_loop.reset_compaction_breakers(&session_id);

    let recovered_run_id = run_id(10);
    let recovered_trigger_id = message_id(170);
    append_user_turn_for_run(
        &store,
        &session_id,
        &recovered_run_id,
        recovered_trigger_id.clone(),
        "recover after anti-thrashing reset",
    );
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_overflow_loop_with(
        &run_loop,
        &model,
        &store,
        &session_id,
        &recovered_run_id,
        &recovered_trigger_id,
        &turns,
    );

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "reset should clear the anti-thrashing guard, got {result:?}"
    );
    assert_eq!(server.handled(), 9);
}

#[test]
fn reset_compaction_breakers_allows_auto_compaction_again() {
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_summary("not a real summary 1"),
        canned_context_limit(),
        canned_summary("not a real summary 2"),
        canned_context_limit(),
        canned_summary("not a real summary 3"),
        canned_context_limit(),
        canned_summary(&valid_summary("Recovered after reset.")),
        canned_stream_completion("reset recovery completed"),
    ]);
    let store = Arc::new(Mutex::new(oversize_session()));
    let session_id = session_id();
    let model = resolved_model(server.base_url());
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());

    for attempt in 1..=3 {
        let run_id = run_id(attempt + 1);
        let trigger_id = message_id(140 + attempt);
        append_user_turn_for_run(
            &store,
            &session_id,
            &run_id,
            trigger_id.clone(),
            &format!("malformed before reset {attempt}"),
        );
        let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
        let result = run_overflow_loop_with(
            &run_loop,
            &model,
            &store,
            &session_id,
            &run_id,
            &trigger_id,
            &turns,
        );

        assert_malformed_failure(result, &format!("malformed summary attempt {attempt}"));
    }

    run_loop.reset_compaction_breakers(&session_id);

    let recovered_run_id = run_id(9);
    let recovered_trigger_id = message_id(149);
    append_user_turn_for_run(
        &store,
        &session_id,
        &recovered_run_id,
        recovered_trigger_id.clone(),
        "recover after breaker reset",
    );
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let result = run_overflow_loop_with(
        &run_loop,
        &model,
        &store,
        &session_id,
        &recovered_run_id,
        &recovered_trigger_id,
        &turns,
    );

    assert!(
        matches!(result, RunLoopResult::Completed(_)),
        "reset should allow auto-compaction to run again, got {result:?}"
    );
    assert_eq!(server.handled(), 9);
}

#[test]
fn transient_summary_failure_cooldown_gates_next_auto_compaction() {
    let server = FakeProviderServer::start(vec![
        canned_context_limit(),
        canned_provider_error(500, "summary model unavailable"),
        canned_context_limit(),
    ]);
    let store = Arc::new(Mutex::new(oversize_session()));
    let session_id = session_id();
    let model = resolved_model(server.base_url());
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());

    let first_run_id = run_id(1);
    let first_trigger_id = message_id(99);
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let first = run_overflow_loop_with(
        &run_loop,
        &model,
        &store,
        &session_id,
        &first_run_id,
        &first_trigger_id,
        &turns,
    );

    assert!(
        matches!(
            first,
            RunLoopResult::Failed(nav_harness::models::OpenAiCompletionsError::Provider(ref error))
            if error.status == 500
        ),
        "transient summary failure should surface the provider error, got {first:?}"
    );

    let blocked_run_id = run_id(11);
    let blocked_trigger_id = message_id(171);
    append_user_turn_for_run(
        &store,
        &session_id,
        &blocked_run_id,
        blocked_trigger_id.clone(),
        "blocked during transient cooldown",
    );
    let turns = store.lock().unwrap().try_turns(&session_id).unwrap();
    let blocked = run_overflow_loop_with(
        &run_loop,
        &model,
        &store,
        &session_id,
        &blocked_run_id,
        &blocked_trigger_id,
        &turns,
    );

    let RunLoopResult::Failed(nav_harness::models::OpenAiCompletionsError::MalformedResponse {
        message,
    }) = blocked
    else {
        panic!("cooldown should block auto-compaction, got {blocked:?}");
    };
    assert_eq!(message, TRANSIENT_COOLDOWN_WARNING);
    assert_eq!(
        server.handled(),
        3,
        "cooldown should stop before a second summary request"
    );
}

fn run_overflow_loop(
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    message_id: &MessageId,
    turns: &[ModelTurn],
) -> RunLoopResult {
    run_overflow_loop_with_override(model, None, store, session_id, run_id, message_id, turns)
}

fn run_overflow_loop_with_override(
    model: &ResolvedModelConfig,
    compaction_model_resolver: Option<&ModelResolver>,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    message_id: &MessageId,
    turns: &[ModelTurn],
) -> RunLoopResult {
    let run_loop = RunLoop::with_client(OpenAiCompletionsClient::new());
    run_overflow_loop_with_resolver(
        &run_loop,
        model,
        compaction_model_resolver,
        store,
        session_id,
        (run_id, message_id),
        turns,
    )
}

fn run_overflow_loop_with(
    run_loop: &RunLoop,
    model: &ResolvedModelConfig,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    message_id: &MessageId,
    turns: &[ModelTurn],
) -> RunLoopResult {
    run_overflow_loop_with_resolver(
        run_loop,
        model,
        None,
        store,
        session_id,
        (run_id, message_id),
        turns,
    )
}

fn run_overflow_loop_with_resolver(
    run_loop: &RunLoop,
    model: &ResolvedModelConfig,
    compaction_model_resolver: Option<&ModelResolver>,
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run: (&RunId, &MessageId),
    turns: &[ModelTurn],
) -> RunLoopResult {
    let registry = ToolRegistry::default();
    let context = ToolContext::default();
    let mut ids = TestIds::default();

    run_loop.run(
        model,
        RunLoopRequest {
            session_id,
            run_id: run.0,
            message_id: run.1,
            turns,
            tool_registry: &registry,
            tool_preset: ToolPreset::Coding,
            tool_context: &context,
            session_store: Some(store),
            pending_confirmations: None,
            compaction_model_resolver,
            cancellation_token: OpenAiCompletionsCancellationToken::new(),
        },
        &mut ids,
        |_events| {},
    )
}

fn append_user_turn_for_run(
    store: &Arc<Mutex<SessionStore>>,
    session_id: &SessionId,
    run_id: &RunId,
    message_id: MessageId,
    text: &str,
) {
    let store = store.lock().unwrap();
    store.start_run(session_id, run_id.clone()).unwrap();
    store
        .append_turn(run_id, message_id, ModelTurn::user_text(text))
        .unwrap();
}

fn assert_malformed_failure(result: RunLoopResult, context: &str) {
    assert!(
        matches!(
            result,
            RunLoopResult::Failed(
                nav_harness::models::OpenAiCompletionsError::MalformedResponse { .. }
            )
        ),
        "{context} should fail with a malformed-response error, got {result:?}"
    );
}

fn oversize_session() -> SessionStore {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id(1);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    for index in 0..12 {
        let turn = if index % 2 == 0 {
            ModelTurn::user_text(format!("user turn {index} with a long oversize body"))
        } else {
            ModelTurn::assistant_text(format!("assistant turn {index} with a long oversize body"))
        };
        store.append_turn(&run_id, message_id(index), turn).unwrap();
    }
    store
        .append_turn(
            &run_id,
            message_id(99),
            ModelTurn::user_text("please finish the overflow handler"),
        )
        .unwrap();

    store
}

fn oversize_session_with_strippable_summary_payload(oversized_result: &str) -> SessionStore {
    let store = SessionStore::default();
    let session_id = session_id();
    let run_id = run_id(1);

    store.create_session(session_id.clone()).unwrap();
    store.start_run(&session_id, run_id.clone()).unwrap();
    for index in 0..8 {
        let turn = if index % 2 == 0 {
            ModelTurn::user_text(format!("user turn {index} with a long oversize body"))
        } else {
            ModelTurn::assistant_text(format!("assistant turn {index} with a long oversize body"))
        };
        store.append_turn(&run_id, message_id(index), turn).unwrap();
    }
    store
        .append_turn(
            &run_id,
            message_id(20),
            ModelTurn {
                role: ModelTurnRole::User,
                parts: vec![
                    TurnPart::Text {
                        text: "[image elided]".to_string(),
                        synthetic: Some(true),
                    },
                    TurnPart::Text {
                        text: "Please continue from the screenshot.".to_string(),
                        synthetic: None,
                    },
                ],
            },
        )
        .unwrap();
    store
        .append_turn(
            &run_id,
            message_id(21),
            ModelTurn::tool_result("tc1", oversized_result),
        )
        .unwrap();
    store
        .append_turn(
            &run_id,
            message_id(98),
            ModelTurn::assistant_text("tail assistant turn"),
        )
        .unwrap();
    store
        .append_turn(
            &run_id,
            message_id(99),
            ModelTurn::user_text("please finish the overflow handler"),
        )
        .unwrap();

    store
}

fn continuation_count(turns: &[ModelTurn]) -> usize {
    turns
        .iter()
        .filter(|turn| turn.text_content() == OVERFLOW_CONTINUATION_TEXT)
        .count()
}

fn synthetic_user_text_count(turns: &[ModelTurn], text: &str) -> usize {
    turns
        .iter()
        .filter(|turn| {
            turn.role == ModelTurnRole::User
                && turn.text_content() == text
                && turn.parts.iter().any(|part| {
                    matches!(
                        part,
                        nav_harness::sessions::TurnPart::Text {
                            synthetic: Some(true),
                            ..
                        }
                    )
                })
        })
        .count()
}

fn last_user_turn(turns: &[ModelTurn]) -> Option<&ModelTurn> {
    turns
        .iter()
        .rev()
        .find(|turn| turn.role == ModelTurnRole::User)
}

fn resolved_model(base_url: &str) -> ResolvedModelConfig {
    resolved_model_with_id(base_url, "overflow-model")
}

fn resolved_model_with_id(base_url: &str, model_id: &str) -> ResolvedModelConfig {
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
                id: model_id.to_string(),
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
            model: model_id.to_string(),
        }),
        providers,
        ..ModelSettings::default()
    })
    .resolve_default()
    .unwrap()
}

fn compaction_override_resolver(base_url: &str, model_id: &str) -> ModelResolver {
    let mut providers = BTreeMap::new();
    providers.insert(
        "summary-provider".to_string(),
        ProviderConfig {
            name: None,
            api: ApiKind::OpenAiCompletions,
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
                context_window: None,
                max_tokens: None,
                compat: Default::default(),
            }],
            compat: Default::default(),
        },
    );
    let mut settings = ModelSettings {
        providers,
        ..ModelSettings::default()
    };
    settings.compaction.model_override = Some(ModelRef {
        provider: "summary-provider".to_string(),
        model: model_id.to_string(),
    });

    ModelResolver::new(settings)
}

#[derive(Clone)]
struct CannedResponse {
    status: u16,
    content_type: &'static str,
    body: String,
}

fn canned_context_limit() -> CannedResponse {
    CannedResponse {
        status: 400,
        content_type: "application/json",
        body: r#"{"error":{"message":"This model's maximum context length is 8192 tokens","code":"context_length_exceeded"}}"#
            .to_string(),
    }
}

fn canned_summary(summary: &str) -> CannedResponse {
    let escaped = summary.replace('\\', "\\\\").replace('\n', "\\n");
    CannedResponse {
        status: 200,
        content_type: "application/json",
        body: format!(
            "{{\"choices\":[{{\"message\":{{\"role\":\"assistant\",\"content\":\"{escaped}\"}}}}]}}"
        ),
    }
}

fn canned_provider_error(status: u16, message: &str) -> CannedResponse {
    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
    CannedResponse {
        status,
        content_type: "application/json",
        body: format!(
            r#"{{"error":{{"message":"{escaped}","type":"server_error","code":"server_error"}}}}"#
        ),
    }
}

fn valid_summary(active_task: &str) -> String {
    format!(
        r#"## Active Task
{active_task}

## Goal
Finish the overflow recovery behavior.

## Constraints & Preferences
Use TDD and keep the run-loop behavior observable.

## Completed Actions
1. Triggered overflow recovery - summary generated [tool: model]

## Active State
The test session is retrying after compaction.

## In Progress
Retry the original user request after compaction.

## Blocked
None.

## Key Decisions
Reject malformed summaries before they enter replay."#
    )
}

fn long_valid_summary() -> String {
    let padding = "x".repeat(900);
    valid_summary(&format!(
        "Continue after a deliberately low-savings compaction. {padding}"
    ))
}

fn canned_stream_completion(text: &str) -> CannedResponse {
    let body = format!(
        "data: {{\"id\":\"chatcmpl_retry\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"id\":\"chatcmpl_retry\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
    );
    CannedResponse {
        status: 200,
        content_type: "text/event-stream",
        body,
    }
}

/// A fake provider that answers a fixed queue of canned responses, one per
/// incoming connection, so a single run-loop pass can be driven through
/// rejection, summary generation, and a successful retry.
struct FakeProviderServer {
    base_url: String,
    handled: Arc<AtomicUsize>,
    request_bodies: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
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
        let request_bodies = Arc::new(Mutex::new(Vec::new()));
        let request_bodies_in_thread = Arc::clone(&request_bodies);

        let handle = thread::spawn(move || {
            for response in responses {
                // Bound the wait so a regression that stops retrying fails the
                // test cleanly instead of hanging this thread (and its join) on
                // a connection that never arrives.
                let Some(mut stream) = accept_before(&listener, Duration::from_secs(10)) else {
                    return;
                };
                let body = drain_http_request(&mut stream);
                request_bodies_in_thread.lock().unwrap().push(body);

                let header = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    reason_phrase(response.status),
                    response.content_type,
                    response.body.len(),
                );
                stream
                    .write_all(header.as_bytes())
                    .expect("response header should write");
                stream
                    .write_all(response.body.as_bytes())
                    .expect("response body should write");
                stream.flush().expect("response should flush");
                handled_in_thread.fetch_add(1, Ordering::SeqCst);
            }
        });

        Self {
            base_url,
            handled,
            request_bodies,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn handled(&self) -> usize {
        self.handled.load(Ordering::SeqCst)
    }

    fn request_bodies(&self) -> Vec<String> {
        self.request_bodies.lock().unwrap().clone()
    }
}

impl Drop for FakeProviderServer {
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
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("accepted stream should set blocking");
                return Some(stream);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fake server accept failed: {error}"),
        }
    }
}

fn drain_http_request(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream.try_clone().expect("stream should clone"));
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).expect("header should read") == 0 {
            return String::new();
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).expect("body should read");
    String::from_utf8_lossy(&body).into_owned()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Status",
    }
}

#[derive(Default)]
struct TestIds {
    next_event: u64,
    next_tool_call: u64,
}

impl HarnessEventIdSource for TestIds {
    fn next_event_id(&mut self) -> EventId {
        self.next_event += 1;
        EventId::try_new(format!("019f2f6f-f178-7a72-9f2a-{:012x}", self.next_event)).unwrap()
    }

    fn next_tool_call_id(&mut self) -> ToolCallId {
        self.next_tool_call += 1;
        ToolCallId::try_new(format!(
            "019f2f6f-f178-7a72-9f2b-{:012x}",
            self.next_tool_call
        ))
        .unwrap()
    }

    fn next_approval_id(&mut self) -> ApprovalId {
        ApprovalId::try_new("019f2f6f-f178-7a72-9f2c-000000000001").unwrap()
    }
}

fn session_id() -> SessionId {
    SessionId::try_new("019f2f6f-f178-7a72-9f28-0000000000a1").unwrap()
}

fn run_id(suffix: u64) -> RunId {
    RunId::try_new(format!("019f2f6f-f178-7a72-9f28-{suffix:012x}")).unwrap()
}

fn message_id(suffix: u64) -> MessageId {
    MessageId::try_new(format!("019f2f6f-f178-7a72-9f29-{suffix:012x}")).unwrap()
}
