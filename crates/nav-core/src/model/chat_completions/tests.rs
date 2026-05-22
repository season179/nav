//! Chat Completions transport tests.
//!
//! Wire-format coverage (request body, response parsing) lands with C1/C2.
//! F1 added the constructor coverage and F2 the SSE normalization
//! fixture-driven tests below.

use super::delta::ChatCompletionsAccumulator;
use super::*;
use crate::context::{ModelConfig, ProviderConfig, Settings};
use crate::model::ResponseEnvelope;
use crate::model::auth::resolve_provider;
use crate::model::responses::{
    ResponseCollector, ResponsesError, assistant_text, function_calls_from,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

fn settings_with_one_catalog_entry() -> Settings {
    let mut models = BTreeMap::new();
    models.insert("glm-5.1".to_string(), ModelConfig::default());
    let provider = ProviderConfig {
        name: Some("Z.AI".to_string()),
        base_url: Some("https://api.z.ai/v1".to_string()),
        api_key: Some("sk-zai-literal".to_string()),
        headers: None,
        models,
    };
    let mut catalog = BTreeMap::new();
    catalog.insert("z.ai".to_string(), provider);
    Settings {
        providers: Some(catalog),
        ..Settings::default()
    }
}

#[test]
fn constructor_accepts_resolved_provider() {
    let settings = settings_with_one_catalog_entry();
    let resolved = resolve_provider(Some("z.ai/glm-5.1"), &settings).unwrap();
    let client = reqwest::Client::new();
    let transport = ChatCompletionsTransport::new(
        client,
        resolved,
        Duration::from_secs(60),
        RetryPolicy::default(),
    );
    // The transport is held as `dyn ResponsesTransport` by the agent loop;
    // this coercion confirms the trait is implemented and the constructor
    // signature matches what `OpenAiTransport` offers.
    let _erased: &dyn ResponsesTransport = &transport;
}

// ── SSE fixture-driven tests (F2) ────────────────────────────
//
// Each test loads an `.sse` fixture, replays its chunks through the
// accumulator, folds the normalized events through `ResponseCollector`,
// and checks the resulting envelope against the sidecar `.json`
// expectations. Same path the production driver walks (minus the network)
// so a fixture regression on either side is caught.

const TEXT_ONLY_SSE: &str =
    include_str!("../../../tests/fixtures/chat_completions_sse/text_only.sse");
const SINGLE_TOOL_CALL_SSE: &str =
    include_str!("../../../tests/fixtures/chat_completions_sse/single_tool_call.sse");
const PARALLEL_TOOL_CALLS_SSE: &str =
    include_str!("../../../tests/fixtures/chat_completions_sse/parallel_tool_calls.sse");
const TEXT_THEN_TOOL_CALL_SSE: &str =
    include_str!("../../../tests/fixtures/chat_completions_sse/text_then_tool_call.sse");
const CONTEXT_OVERFLOW_SSE: &str =
    include_str!("../../../tests/fixtures/chat_completions_sse/context_overflow_error.sse");

struct Fixture {
    envelope: ResponseEnvelope,
}

fn parse_sse_chunks(sse: &str) -> Vec<Value> {
    // SSE frames are separated by blank lines. Strip the `data:` prefix on
    // each line and skip blanks plus the `[DONE]` sentinel — the rest is the
    // chat.completion.chunk JSON.
    let mut chunks = Vec::new();
    for frame in sse.split("\n\n") {
        for line in frame.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            chunks.push(serde_json::from_str(data).expect("SSE fixture has invalid JSON"));
        }
    }
    chunks
}

fn run_fixture(sse: &str) -> Result<Fixture, ResponsesError> {
    let mut accumulator = ChatCompletionsAccumulator::new();
    let mut collector = ResponseCollector::default();
    for chunk in parse_sse_chunks(sse) {
        for event in accumulator.push_chunk(&chunk)? {
            let done = collector
                .push_event(&event, "fixture")
                .map_err(ResponsesError::Other)?;
            if done {
                let envelope = collector.finish("fixture").map_err(ResponsesError::Other)?;
                return Ok(Fixture { envelope });
            }
        }
    }
    for event in accumulator.finalize() {
        let done = collector
            .push_event(&event, "fixture")
            .map_err(ResponsesError::Other)?;
        if done {
            break;
        }
    }
    let envelope = collector.finish("fixture").map_err(ResponsesError::Other)?;
    Ok(Fixture { envelope })
}

#[test]
fn fixture_text_only_produces_assistant_text() {
    let fx = run_fixture(TEXT_ONLY_SSE).expect("fixture should collect cleanly");
    assert_eq!(
        assistant_text(&fx.envelope).as_deref(),
        Some("Hello! I'm ready to help.")
    );
    assert!(
        function_calls_from(&fx.envelope).unwrap().is_empty(),
        "pure text stream should not produce tool calls",
    );
}

#[test]
fn fixture_single_tool_call_reassembles_arguments() {
    let fx = run_fixture(SINGLE_TOOL_CALL_SSE).expect("fixture should collect cleanly");
    assert!(assistant_text(&fx.envelope).is_none());
    let calls = function_calls_from(&fx.envelope).expect("tool calls should parse");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "call_read_1");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments["path"], "main.rs");
}

#[test]
fn fixture_parallel_tool_calls_emit_in_index_order() {
    let fx = run_fixture(PARALLEL_TOOL_CALLS_SSE).expect("fixture should collect cleanly");
    let calls = function_calls_from(&fx.envelope).expect("tool calls should parse");
    assert_eq!(calls.len(), 2);
    // The acceptance criterion: index 0 first, then index 1.
    assert_eq!(calls[0].call_id, "call_read_a");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments["path"], "a.rs");
    assert_eq!(calls[1].call_id, "call_bash_b");
    assert_eq!(calls[1].name, "bash");
    assert_eq!(calls[1].arguments["command"], "ls");
}

#[test]
fn fixture_text_then_tool_call_yields_message_and_call() {
    let fx = run_fixture(TEXT_THEN_TOOL_CALL_SSE).expect("fixture should collect cleanly");
    assert_eq!(
        assistant_text(&fx.envelope).as_deref(),
        Some("Let me read that file.")
    );
    let calls = function_calls_from(&fx.envelope).expect("tool calls should parse");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id, "call_read_c");
    assert_eq!(calls[0].arguments["path"], "config.toml");
}

#[test]
fn fixture_context_overflow_surfaces_as_context_window_exceeded() {
    let err = match run_fixture(CONTEXT_OVERFLOW_SSE) {
        Err(err) => err,
        Ok(_) => panic!("overflow chunk must abort"),
    };
    match err {
        ResponsesError::ContextWindowExceeded { message } => {
            assert!(message.contains("8192"), "got {message:?}");
            assert!(message.contains("12000"), "got {message:?}");
        }
        other => panic!("expected ContextWindowExceeded, got {other}"),
    }
}

#[test]
fn fixture_done_sentinel_does_not_synthesize_empty_message() {
    // Pure-text fixture ends with `data: [DONE]` after the usage chunk.
    // Verify the trailing sentinel doesn't leave behind a phantom empty
    // assistant message item.
    let fx = run_fixture(TEXT_ONLY_SSE).unwrap();
    let text = assistant_text(&fx.envelope).unwrap();
    assert!(!text.is_empty());
    // Only one message item — no synthetic empty trailer.
    let raw: Vec<Value> = crate::model::responses::into_raw_output(fx.envelope);
    let messages: Vec<&Value> = raw
        .iter()
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("message"))
        .collect();
    assert_eq!(messages.len(), 1);
}
