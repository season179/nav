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
        HarnessEvent::ToolCallCompleted {
            run_id,
            tool_call_id,
            name,
            arguments,
            metadata,
        } => BackendEvent::ToolCallCompleted {
            run_id,
            tool_call_id,
            name,
            arguments,
            metadata: Some(provider_metadata(metadata)),
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
