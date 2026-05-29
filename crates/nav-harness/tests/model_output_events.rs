use nav_harness::events::{
    HarnessEvent, HarnessEventEnvelope, HarnessEventIdSource, ModelOutputContext,
    OpenAiStreamEventMapper,
};
use nav_harness::models::OpenAiCompletionsResponseParser;
use nav_types::{ApprovalId, EventId, MessageId, RunId, ToolCallId};

#[test]
fn normal_text_streaming_emits_model_output_events() {
    let mut mapper = model_output_mapper();
    let mut ids = TestIds::default();

    let mut events = Vec::new();
    for raw_event in [
        r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"content":"hel"},"finish_reason":null}]}"#,
        r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":null}]}"#,
        r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
        "data: [DONE]",
    ] {
        events.extend(mapper.map_stream_result(
            OpenAiCompletionsResponseParser::parse_stream_event(raw_event, "sk-secret"),
            &mut ids,
        ));
    }

    assert_eq!(
        event_types(&events),
        vec![
            "model.text_delta",
            "model.text_delta",
            "message.completed",
            "run.completed",
        ]
    );
    assert_eq!(events[0].event_id, event_id(1));

    match &events[0].event {
        HarnessEvent::ModelTextDelta {
            run_id: event_run_id,
            message_id: event_message_id,
            delta,
            metadata,
        } => {
            assert_eq!(event_run_id, &run_id());
            assert_eq!(event_message_id, &message_id());
            assert_eq!(delta, "hel");
            assert_eq!(metadata.provider_id, "compatible-gateway");
            assert_eq!(metadata.configured_model_id, "vendor/model-large");
            assert_eq!(metadata.provider_response_id.as_deref(), Some("chatcmpl_1"));
            assert_eq!(metadata.provider_model.as_deref(), Some("actual-model"));
            assert_eq!(metadata.choice_index, Some(0));
        }
        event => panic!("expected text delta, got {event:?}"),
    }

    match &events[2].event {
        HarnessEvent::MessageCompleted {
            run_id: event_run_id,
            message_id: event_message_id,
            finish_reason,
            ..
        } => {
            assert_eq!(event_run_id, &run_id());
            assert_eq!(event_message_id, &message_id());
            assert_eq!(finish_reason.as_deref(), Some("stop"));
        }
        event => panic!("expected message completion, got {event:?}"),
    }

    match &events[3].event {
        HarnessEvent::RunCompleted { metadata, .. } => {
            assert_eq!(metadata.provider_response_id.as_deref(), Some("chatcmpl_1"));
            assert_eq!(metadata.provider_model.as_deref(), Some("actual-model"));
            let usage = metadata.usage.as_ref().expect("usage should be preserved");
            assert_eq!(usage.prompt_tokens, Some(3));
            assert_eq!(usage.completion_tokens, Some(2));
            assert_eq!(usage.total_tokens, Some(5));
        }
        event => panic!("expected run completion, got {event:?}"),
    }

    assert!(
        !format!("{events:?}").contains("sk-secret"),
        "event debug output must not leak provider secrets"
    );
}

#[test]
fn provider_error_before_output_emits_provider_error_event() {
    let mut mapper = model_output_mapper();
    let mut ids = TestIds::default();

    let events = mapper.map_stream_result(
        Err(OpenAiCompletionsResponseParser::parse_error_response(
            429,
            r#"{"error":{"message":"rate limited sk-secret","type":"rate_limit_error","code":"too_many_requests"}}"#,
            "sk-secret",
        )),
        &mut ids,
    );

    assert_eq!(event_types(&events), vec!["provider.error"]);

    match &events[0].event {
        HarnessEvent::ProviderError {
            run_id: event_run_id,
            status,
            message,
            error_type,
            code,
            metadata,
        } => {
            assert_eq!(event_run_id, &run_id());
            assert_eq!(*status, Some(429));
            assert_eq!(message, "rate limited <redacted>");
            assert_eq!(error_type.as_deref(), Some("rate_limit_error"));
            assert_eq!(code.as_deref(), Some("too_many_requests"));
            assert_eq!(metadata.provider_id, "compatible-gateway");
            assert_eq!(metadata.configured_model_id, "vendor/model-large");
            assert_eq!(metadata.provider_response_id, None);
            assert_eq!(metadata.provider_model, None);
            assert_eq!(metadata.choice_index, None);
        }
        event => panic!("expected provider error, got {event:?}"),
    }

    assert!(
        !format!("{events:?}").contains("sk-secret"),
        "event debug output must not leak provider secrets"
    );
}

#[test]
fn context_limit_error_preserves_status_and_code_in_provider_error_event() {
    let mut mapper = model_output_mapper();
    let mut ids = TestIds::default();

    let events = mapper.map_stream_result(
        Err(OpenAiCompletionsResponseParser::parse_error_response(
            400,
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
            "sk-secret",
        )),
        &mut ids,
    );

    assert_eq!(event_types(&events), vec!["provider.error"]);

    match &events[0].event {
        HarnessEvent::ProviderError {
            status,
            code,
            message,
            ..
        } => {
            assert_eq!(*status, Some(400));
            assert_eq!(code.as_deref(), Some("context_length_exceeded"));
            assert_eq!(
                message,
                "This model's maximum context length is 8192 tokens."
            );
        }
        event => panic!("expected provider error, got {event:?}"),
    }
}

#[test]
fn streamed_context_limit_error_frame_is_classified() {
    let mut mapper = model_output_mapper();
    let mut ids = TestIds::default();

    // A gateway can deliver a context-overflow error inside a 200 SSE stream
    // rather than as an initial HTTP 400; it must still classify as a limit.
    let events = mapper.map_stream_result(
        OpenAiCompletionsResponseParser::parse_stream_event(
            r#"data: {"error":{"message":"This model's maximum context length is 8192 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
            "sk-secret",
        ),
        &mut ids,
    );

    assert_eq!(event_types(&events), vec!["provider.error"]);

    match &events[0].event {
        HarnessEvent::ProviderError { status, code, .. } => {
            assert_eq!(*status, Some(400));
            assert_eq!(code.as_deref(), Some("context_length_exceeded"));
        }
        event => panic!("expected provider error, got {event:?}"),
    }
}

#[test]
fn provider_error_after_partial_output_preserves_stream_metadata() {
    let mut mapper = model_output_mapper();
    let mut ids = TestIds::default();

    let mut events = mapper.map_stream_result(
        OpenAiCompletionsResponseParser::parse_stream_event(
            r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}]}"#,
            "sk-secret",
        ),
        &mut ids,
    );
    events.extend(mapper.map_stream_result(
        OpenAiCompletionsResponseParser::parse_stream_event(
            r#"data: {"error":{"message":"stream failed sk-secret","type":"server_error","code":"upstream_failed"}}"#,
            "sk-secret",
        ),
        &mut ids,
    ));

    assert_eq!(
        event_types(&events),
        vec!["model.text_delta", "provider.error"]
    );

    match &events[1].event {
        HarnessEvent::ProviderError {
            run_id: event_run_id,
            status,
            message,
            error_type,
            code,
            metadata,
        } => {
            assert_eq!(event_run_id, &run_id());
            assert_eq!(*status, None);
            assert_eq!(message, "stream failed <redacted>");
            assert_eq!(error_type.as_deref(), Some("server_error"));
            assert_eq!(code.as_deref(), Some("upstream_failed"));
            assert_eq!(metadata.provider_response_id.as_deref(), Some("chatcmpl_1"));
            assert_eq!(metadata.provider_model.as_deref(), Some("actual-model"));
            assert_eq!(metadata.choice_index, Some(0));
        }
        event => panic!("expected provider error, got {event:?}"),
    }

    assert!(
        !format!("{events:?}").contains("sk-secret"),
        "event debug output must not leak provider secrets"
    );
}

#[test]
fn reasoning_and_tool_call_streaming_emit_internal_events() {
    let mut mapper = model_output_mapper();
    let mut ids = TestIds::default();

    let mut events = Vec::new();
    for raw_event in [
        r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"reasoning_content":"thinking"},"finish_reason":null}]}"#,
        r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_provider_1","type":"function","function":{"name":"shell","arguments":"{\"cmd\":\"ls"}}]},"finish_reason":null}]}"#,
        r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"}"}}]},"finish_reason":"tool_calls"}]}"#,
    ] {
        events.extend(mapper.map_stream_result(
            OpenAiCompletionsResponseParser::parse_stream_event(raw_event, "sk-secret"),
            &mut ids,
        ));
    }

    assert_eq!(
        event_types(&events),
        vec![
            "model.reasoning_delta",
            "tool.call_started",
            "tool.call_delta",
            "tool.call_delta",
            "tool.call_completed",
            "message.completed",
        ]
    );

    match &events[0].event {
        HarnessEvent::ModelReasoningDelta {
            delta, metadata, ..
        } => {
            assert_eq!(delta, "thinking");
            assert_eq!(metadata.provider_response_id.as_deref(), Some("chatcmpl_1"));
        }
        event => panic!("expected reasoning delta, got {event:?}"),
    }

    match &events[1].event {
        HarnessEvent::ToolCallStarted {
            tool_call_id: event_tool_call_id,
            name,
            metadata,
            ..
        } => {
            assert_eq!(event_tool_call_id, &tool_call_id(1));
            assert_eq!(name.as_deref(), Some("shell"));
            assert_eq!(
                metadata.provider_tool_call_id.as_deref(),
                Some("call_provider_1")
            );
        }
        event => panic!("expected tool call start, got {event:?}"),
    }

    match &events[4].event {
        HarnessEvent::ToolCallCompleted {
            tool_call_id: event_tool_call_id,
            name,
            arguments,
            ..
        } => {
            assert_eq!(event_tool_call_id, &tool_call_id(1));
            assert_eq!(name.as_deref(), Some("shell"));
            assert_eq!(arguments, r#"{"cmd":"ls"}"#);
        }
        event => panic!("expected tool call completion, got {event:?}"),
    }
}

#[test]
fn reasoning_aliases_emit_one_reasoning_delta() {
    let mut mapper = model_output_mapper();
    let mut ids = TestIds::default();

    let events = mapper.map_stream_result(
        OpenAiCompletionsResponseParser::parse_stream_event(
            r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"reasoning":"plan"},"finish_reason":null}]}"#,
            "sk-secret",
        ),
        &mut ids,
    );

    assert_eq!(event_types(&events), vec!["model.reasoning_delta"]);

    match &events[0].event {
        HarnessEvent::ModelReasoningDelta { delta, .. } => {
            assert_eq!(delta, "plan");
        }
        event => panic!("expected reasoning delta, got {event:?}"),
    }
}

#[test]
fn tool_call_chunks_are_matched_by_index_or_provider_id() {
    let mut mapper = model_output_mapper();
    let mut ids = TestIds::default();

    let mut events = mapper.map_stream_result(
        OpenAiCompletionsResponseParser::parse_stream_event(
            r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_provider_1","type":"function","function":{"name":"shell","arguments":"{\"cmd\""}}]},"finish_reason":null}]}"#,
            "sk-secret",
        ),
        &mut ids,
    );
    events.extend(mapper.map_stream_result(
        OpenAiCompletionsResponseParser::parse_stream_event(
            r#"data: {"id":"chatcmpl_1","model":"actual-model","choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_provider_1","function":{"arguments":":\"ls\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            "sk-secret",
        ),
        &mut ids,
    ));

    assert_eq!(
        event_types(&events),
        vec![
            "tool.call_started",
            "tool.call_delta",
            "tool.call_delta",
            "tool.call_completed",
            "message.completed",
        ]
    );

    match &events[3].event {
        HarnessEvent::ToolCallCompleted {
            tool_call_id: event_tool_call_id,
            arguments,
            ..
        } => {
            assert_eq!(event_tool_call_id, &tool_call_id(1));
            assert_eq!(arguments, r#"{"cmd":"ls"}"#);
        }
        event => panic!("expected tool call completion, got {event:?}"),
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
        event_id(self.next_event)
    }

    fn next_tool_call_id(&mut self) -> ToolCallId {
        self.next_tool_call += 1;
        ToolCallId::try_new(format!(
            "019f2f6f-f178-7a72-9f28-{:012x}",
            self.next_tool_call
        ))
        .expect("test tool call id should be UUIDv7-shaped")
    }

    fn next_approval_id(&mut self) -> ApprovalId {
        ApprovalId::try_new("019f2f6f-f178-7a72-9f28-000000000040")
            .expect("test approval id should be UUIDv7-shaped")
    }
}

fn model_output_mapper() -> OpenAiStreamEventMapper {
    OpenAiStreamEventMapper::new(ModelOutputContext {
        run_id: run_id(),
        message_id: message_id(),
        provider_id: "compatible-gateway".to_string(),
        configured_model_id: "vendor/model-large".to_string(),
    })
}

fn event_types(events: &[HarnessEventEnvelope]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| event.event.event_type())
        .collect()
}

fn event_id(index: u64) -> EventId {
    EventId::try_new(format!("019f2f6f-f178-7a72-9f28-{index:012x}"))
        .expect("test event id should be UUIDv7-shaped")
}

fn tool_call_id(index: u64) -> ToolCallId {
    ToolCallId::try_new(format!("019f2f6f-f178-7a72-9f28-{index:012x}"))
        .expect("test tool call id should be UUIDv7-shaped")
}

fn run_id() -> RunId {
    RunId::try_new("019f2f6f-f178-7a72-9f28-000000000101")
        .expect("test run id should be UUIDv7-shaped")
}

fn message_id() -> MessageId {
    MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000202")
        .expect("test message id should be UUIDv7-shaped")
}
