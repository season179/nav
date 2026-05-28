use nav_harness::events::{
    HarnessEvent, HarnessEventEnvelope, ProviderEventMetadata as HarnessProviderEventMetadata,
    ProviderUsage as HarnessProviderUsage,
};
use nav_protocol::{BackendEvent, EventEnvelope, ProviderEventMetadata, ProviderUsage};
use nav_types::SessionId;

pub(super) fn harness_events_to_backend_events(
    session_id: &SessionId,
    events: Vec<HarnessEventEnvelope>,
) -> Vec<EventEnvelope> {
    events
        .into_iter()
        .map(|event| EventEnvelope {
            event_id: event.event_id,
            session_id: session_id.clone(),
            event: harness_event_to_backend_event(event.event),
        })
        .collect()
}

fn harness_event_to_backend_event(event: HarnessEvent) -> BackendEvent {
    match event {
        HarnessEvent::ModelTextDelta {
            run_id,
            message_id,
            delta,
            metadata,
        } => BackendEvent::ModelTextDelta {
            run_id,
            message_id,
            delta,
            metadata: provider_metadata(metadata),
        },
        HarnessEvent::ModelReasoningDelta {
            run_id,
            message_id,
            delta,
            metadata,
        } => BackendEvent::ModelReasoningDelta {
            run_id,
            message_id,
            delta,
            metadata: provider_metadata(metadata),
        },
        HarnessEvent::MessageCompleted {
            run_id,
            message_id,
            finish_reason,
            metadata,
        } => BackendEvent::MessageCompleted {
            run_id,
            message_id,
            finish_reason,
            metadata: Some(provider_metadata(metadata)),
        },
        HarnessEvent::ToolCallStarted {
            run_id,
            tool_call_id,
            name,
            metadata,
        } => BackendEvent::ToolCallStarted {
            run_id,
            tool_call_id,
            name,
            metadata: Some(provider_metadata(metadata)),
        },
        HarnessEvent::ToolCallDelta {
            run_id,
            tool_call_id,
            arguments_delta,
            metadata,
        } => BackendEvent::ToolCallDelta {
            run_id,
            tool_call_id,
            arguments_delta,
            metadata: provider_metadata(metadata),
        },
        HarnessEvent::ToolOutputDelta {
            run_id,
            tool_call_id,
            stream,
            chunk,
        } => BackendEvent::ToolOutputDelta {
            run_id,
            tool_call_id,
            stream,
            chunk,
        },
        HarnessEvent::ToolCallCompleted {
            run_id,
            tool_call_id,
            name,
            arguments,
            output,
            output_lossy,
            metadata,
        } => BackendEvent::ToolCallCompleted {
            run_id,
            tool_call_id,
            name,
            arguments,
            output,
            output_lossy,
            metadata: Some(provider_metadata(metadata)),
        },
        HarnessEvent::ToolCallFailed {
            run_id,
            tool_call_id,
            name,
            error_message,
            metadata,
        } => BackendEvent::ToolCallFailed {
            run_id,
            tool_call_id,
            name,
            error_message,
            metadata: Some(provider_metadata(metadata)),
        },
        HarnessEvent::ToolApprovalRequested {
            run_id,
            tool_call_id,
            approval_id,
            tool_name,
            reason,
            arguments_summary,
            risk_class,
        } => BackendEvent::ToolApprovalRequested {
            run_id,
            tool_call_id,
            approval_id,
            tool_name,
            reason,
            arguments_summary,
            risk_class,
        },
        HarnessEvent::FileChanged {
            file_change_id,
            path,
        } => BackendEvent::FileChanged {
            file_change_id,
            path,
        },
        HarnessEvent::ProviderError {
            run_id,
            status,
            message,
            error_type,
            code,
            metadata,
        } => BackendEvent::ProviderError {
            run_id,
            status,
            message,
            error_type,
            code,
            metadata: provider_metadata(metadata),
        },
        HarnessEvent::RunCompleted { run_id, metadata } => BackendEvent::RunCompleted {
            run_id,
            metadata: Some(provider_metadata(metadata)),
        },
    }
}

fn provider_metadata(metadata: HarnessProviderEventMetadata) -> ProviderEventMetadata {
    ProviderEventMetadata {
        provider_id: metadata.provider_id,
        configured_model_id: metadata.configured_model_id,
        provider_response_id: metadata.provider_response_id,
        provider_model: metadata.provider_model,
        choice_index: metadata.choice_index,
        provider_tool_call_id: metadata.provider_tool_call_id,
        usage: metadata.usage.map(provider_usage),
    }
}

fn provider_usage(usage: HarnessProviderUsage) -> ProviderUsage {
    ProviderUsage {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nav_harness::events::ProviderEventMetadata as HMeta;
    use nav_types::{ApprovalId, EventId, FileChangeId, RunId, ToolCallId};

    fn run_id() -> RunId {
        RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap()
    }

    fn tool_call_id() -> ToolCallId {
        ToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000003").unwrap()
    }

    fn session_id() -> SessionId {
        SessionId::try_new("019f2f6f-f178-7a72-9f28-000000000010").unwrap()
    }

    fn event_id() -> EventId {
        EventId::try_new("019f2f6f-f178-7a72-9f28-000000000020").unwrap()
    }

    fn harness_metadata() -> HMeta {
        HMeta {
            provider_id: "test-provider".to_string(),
            configured_model_id: "test-model".to_string(),
            provider_response_id: None,
            provider_model: None,
            choice_index: None,
            provider_tool_call_id: None,
            usage: None,
        }
    }

    fn map_single(event: HarnessEvent) -> BackendEvent {
        let session_id = session_id();
        let envelopes = harness_events_to_backend_events(
            &session_id,
            vec![HarnessEventEnvelope {
                event_id: event_id(),
                event,
            }],
        );
        envelopes.into_iter().next().unwrap().event
    }

    #[test]
    fn tool_call_failed_maps_to_backend_event() {
        let backend = map_single(HarnessEvent::ToolCallFailed {
            run_id: run_id(),
            tool_call_id: tool_call_id(),
            name: Some("read".to_string()),
            error_message: "file not found".to_string(),
            metadata: harness_metadata(),
        });

        match backend {
            BackendEvent::ToolCallFailed {
                run_id: rid,
                tool_call_id: tcid,
                name,
                error_message,
                metadata,
            } => {
                assert_eq!(rid, run_id());
                assert_eq!(tcid, tool_call_id());
                assert_eq!(name, Some("read".to_string()));
                assert_eq!(error_message, "file not found");
                let meta = metadata.expect("tool.call_failed should include metadata");
                assert_eq!(meta.provider_id, "test-provider");
                assert_eq!(meta.configured_model_id, "test-model");
            }
            other => panic!("expected ToolCallFailed, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_started_maps_preserving_optional_name() {
        let backend = map_single(HarnessEvent::ToolCallStarted {
            run_id: run_id(),
            tool_call_id: tool_call_id(),
            name: None,
            metadata: harness_metadata(),
        });

        match backend {
            BackendEvent::ToolCallStarted { name, .. } => assert_eq!(name, None),
            other => panic!("expected ToolCallStarted, got {other:?}"),
        }
    }

    #[test]
    fn all_harness_events_map_to_backend_events() {
        let events = vec![
            HarnessEvent::ModelTextDelta {
                run_id: run_id(),
                message_id: nav_types::MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000002")
                    .unwrap(),
                delta: "hi".to_string(),
                metadata: harness_metadata(),
            },
            HarnessEvent::ModelReasoningDelta {
                run_id: run_id(),
                message_id: nav_types::MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000002")
                    .unwrap(),
                delta: "think".to_string(),
                metadata: harness_metadata(),
            },
            HarnessEvent::MessageCompleted {
                run_id: run_id(),
                message_id: nav_types::MessageId::try_new("019f2f6f-f178-7a72-9f28-000000000002")
                    .unwrap(),
                finish_reason: Some("stop".to_string()),
                metadata: harness_metadata(),
            },
            HarnessEvent::ToolCallStarted {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                name: Some("read".to_string()),
                metadata: harness_metadata(),
            },
            HarnessEvent::ToolCallDelta {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                arguments_delta: "{\"path\":".to_string(),
                metadata: harness_metadata(),
            },
            HarnessEvent::ToolCallCompleted {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                name: Some("read".to_string()),
                arguments: "{\"path\":\"x\"}".to_string(),
                output: None,
                output_lossy: None,
                metadata: harness_metadata(),
            },
            HarnessEvent::ToolOutputDelta {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                stream: "stdout".to_string(),
                chunk: "hello\n".to_string(),
            },
            HarnessEvent::ToolCallFailed {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                name: Some("read".to_string()),
                error_message: "not found".to_string(),
                metadata: harness_metadata(),
            },
            HarnessEvent::ToolApprovalRequested {
                run_id: run_id(),
                tool_call_id: tool_call_id(),
                approval_id: approval_id(),
                tool_name: "write_file".to_string(),
                reason: "confirm write".to_string(),
                arguments_summary: "{}".to_string(),
                risk_class: Some("mutate".to_string()),
            },
            HarnessEvent::FileChanged {
                file_change_id: file_change_id(),
                path: "notes.md".to_string(),
            },
            HarnessEvent::ProviderError {
                run_id: run_id(),
                status: Some(500),
                message: "err".to_string(),
                error_type: None,
                code: None,
                metadata: harness_metadata(),
            },
            HarnessEvent::RunCompleted {
                run_id: run_id(),
                metadata: harness_metadata(),
            },
        ];

        let session_id = session_id();
        let envelopes: Vec<HarnessEventEnvelope> = events
            .into_iter()
            .enumerate()
            .map(|(i, event)| HarnessEventEnvelope {
                event_id: EventId::try_new(format!("019f2f6f-f178-7a72-9f28-{i:012x}")).unwrap(),
                event,
            })
            .collect();

        let result = harness_events_to_backend_events(&session_id, envelopes);
        assert_eq!(result.len(), 12);
        for envelope in &result {
            assert_eq!(envelope.session_id, session_id);
        }
    }

    fn approval_id() -> ApprovalId {
        ApprovalId::try_new("019f2f6f-f178-7a72-9f28-000000000004").unwrap()
    }

    fn file_change_id() -> FileChangeId {
        FileChangeId::try_new("019f2f6f-f178-7a72-9f28-000000000005").unwrap()
    }
}
