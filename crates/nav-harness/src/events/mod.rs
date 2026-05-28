//! Internal event history and fan-out for frontend replay.
//!
//! SSE is a server concern. This module owns the agent-side event log that any
//! transport can read from.

use std::collections::BTreeMap;

use nav_types::{ApprovalId, EventId, FileChangeId, MessageId, RunId, ToolCallId};

use crate::models::{
    ChatCompletionStreamChoice, ChatCompletionStreamChunk, ChatCompletionStreamEvent,
    ChatCompletionToolCallDelta, ChatCompletionUsage, OpenAiCompletionsError,
    OpenAiCompletionsProviderError, OpenAiCompletionsStreamProviderError,
};

#[derive(Debug, Default)]
pub struct EventLog;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessEventEnvelope {
    pub event_id: EventId,
    pub event: HarnessEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessEvent {
    ModelTextDelta {
        run_id: RunId,
        message_id: MessageId,
        delta: String,
        metadata: ProviderEventMetadata,
    },
    ModelReasoningDelta {
        run_id: RunId,
        message_id: MessageId,
        delta: String,
        metadata: ProviderEventMetadata,
    },
    MessageCompleted {
        run_id: RunId,
        message_id: MessageId,
        finish_reason: Option<String>,
        metadata: ProviderEventMetadata,
    },
    ToolCallStarted {
        run_id: RunId,
        tool_call_id: ToolCallId,
        name: Option<String>,
        metadata: ProviderEventMetadata,
    },
    ToolCallDelta {
        run_id: RunId,
        tool_call_id: ToolCallId,
        arguments_delta: String,
        metadata: ProviderEventMetadata,
    },
    ToolCallCompleted {
        run_id: RunId,
        tool_call_id: ToolCallId,
        name: Option<String>,
        arguments: String,
        metadata: ProviderEventMetadata,
    },
    ToolCallFailed {
        run_id: RunId,
        tool_call_id: ToolCallId,
        name: Option<String>,
        error_message: String,
        metadata: ProviderEventMetadata,
    },
    ToolApprovalRequested {
        run_id: RunId,
        tool_call_id: ToolCallId,
        approval_id: ApprovalId,
        tool_name: String,
        reason: String,
        arguments_summary: String,
        risk_class: Option<String>,
    },
    FileChanged {
        file_change_id: FileChangeId,
        path: String,
    },
    ProviderError {
        run_id: RunId,
        status: Option<u16>,
        message: String,
        error_type: Option<String>,
        code: Option<String>,
        metadata: ProviderEventMetadata,
    },
    RunCompleted {
        run_id: RunId,
        metadata: ProviderEventMetadata,
    },
}

impl HarnessEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::ModelTextDelta { .. } => "model.text_delta",
            Self::ModelReasoningDelta { .. } => "model.reasoning_delta",
            Self::MessageCompleted { .. } => "message.completed",
            Self::ToolCallStarted { .. } => "tool.call_started",
            Self::ToolCallDelta { .. } => "tool.call_delta",
            Self::ToolCallCompleted { .. } => "tool.call_completed",
            Self::ToolCallFailed { .. } => "tool.call_failed",
            Self::ToolApprovalRequested { .. } => "tool.approval_requested",
            Self::FileChanged { .. } => "file.changed",
            Self::ProviderError { .. } => "provider.error",
            Self::RunCompleted { .. } => "run.completed",
        }
    }
}

pub trait HarnessEventIdSource {
    fn next_event_id(&mut self) -> EventId;
    fn next_tool_call_id(&mut self) -> ToolCallId;
    fn next_approval_id(&mut self) -> ApprovalId;

    fn next_file_change_id(&mut self) -> FileChangeId {
        FileChangeId::try_new(self.next_event_id().to_string())
            .expect("generated file change id should be UUIDv7")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOutputContext {
    pub run_id: RunId,
    pub message_id: MessageId,
    pub provider_id: String,
    pub configured_model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEventMetadata {
    pub provider_id: String,
    pub configured_model_id: String,
    pub provider_response_id: Option<String>,
    pub provider_model: Option<String>,
    pub choice_index: Option<u32>,
    pub provider_tool_call_id: Option<String>,
    pub usage: Option<ProviderUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsage {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

impl From<&ChatCompletionUsage> for ProviderUsage {
    fn from(usage: &ChatCompletionUsage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

impl ProviderEventMetadata {
    fn with_provider_tool_call_id(&self, provider_tool_call_id: Option<String>) -> Self {
        Self {
            provider_tool_call_id,
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiStreamEventMapper {
    context: ModelOutputContext,
    message_completed: bool,
    last_metadata: Option<ProviderEventMetadata>,
    tool_calls: BTreeMap<String, ToolCallState>,
    tool_call_aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct ToolCallState {
    tool_call_id: ToolCallId,
    provider_tool_call_id: Option<String>,
    name: Option<String>,
    arguments: String,
    started: bool,
    completed: bool,
}

impl OpenAiStreamEventMapper {
    pub fn new(context: ModelOutputContext) -> Self {
        Self {
            context,
            message_completed: false,
            last_metadata: None,
            tool_calls: BTreeMap::new(),
            tool_call_aliases: BTreeMap::new(),
        }
    }

    pub fn map_stream_result(
        &mut self,
        result: Result<Option<ChatCompletionStreamEvent>, OpenAiCompletionsError>,
        ids: &mut impl HarnessEventIdSource,
    ) -> Vec<HarnessEventEnvelope> {
        match result {
            Ok(Some(ChatCompletionStreamEvent::Chunk(chunk))) => self.map_chunk(chunk, ids),
            Ok(Some(ChatCompletionStreamEvent::Done)) => {
                vec![event_envelope(ids, self.run_completed_event())]
            }
            Ok(None) => Vec::new(),
            Err(error) => vec![event_envelope(ids, self.provider_error_event(error))],
        }
    }

    fn map_chunk(
        &mut self,
        chunk: ChatCompletionStreamChunk,
        ids: &mut impl HarnessEventIdSource,
    ) -> Vec<HarnessEventEnvelope> {
        let mut events = Vec::new();

        if chunk.choices.is_empty() {
            self.last_metadata = Some(self.metadata_for_chunk(&chunk));
            return events;
        }

        for choice in &chunk.choices {
            let metadata = self.metadata_for_choice(&chunk, choice);
            self.last_metadata = Some(metadata.clone());

            if let Some(delta) = choice
                .delta
                .content
                .as_ref()
                .filter(|delta| !delta.is_empty())
            {
                push_event(
                    &mut events,
                    ids,
                    HarnessEvent::ModelTextDelta {
                        run_id: self.context.run_id.clone(),
                        message_id: self.context.message_id.clone(),
                        delta: delta.clone(),
                        metadata: metadata.clone(),
                    },
                );
            }

            if let Some(delta) = choice.delta.reasoning_delta() {
                push_event(
                    &mut events,
                    ids,
                    HarnessEvent::ModelReasoningDelta {
                        run_id: self.context.run_id.clone(),
                        message_id: self.context.message_id.clone(),
                        delta: delta.to_string(),
                        metadata: metadata.clone(),
                    },
                );
            }

            self.map_tool_call_deltas(&choice.delta.tool_calls, &metadata, ids, &mut events);

            if let Some(finish_reason) = &choice.finish_reason
                && !self.message_completed
            {
                if finish_reason == "tool_calls" {
                    self.complete_active_tool_calls(&metadata, ids, &mut events);
                }

                self.message_completed = true;
                push_event(
                    &mut events,
                    ids,
                    HarnessEvent::MessageCompleted {
                        run_id: self.context.run_id.clone(),
                        message_id: self.context.message_id.clone(),
                        finish_reason: Some(finish_reason.clone()),
                        metadata,
                    },
                );
            }
        }

        events
    }

    fn map_tool_call_deltas(
        &mut self,
        tool_call_deltas: &[ChatCompletionToolCallDelta],
        metadata: &ProviderEventMetadata,
        ids: &mut impl HarnessEventIdSource,
        events: &mut Vec<HarnessEventEnvelope>,
    ) {
        for tool_call_delta in tool_call_deltas {
            let key = self.resolve_tool_call_key(tool_call_delta);
            let state = self.tool_calls.entry(key).or_insert_with(|| ToolCallState {
                tool_call_id: ids.next_tool_call_id(),
                provider_tool_call_id: tool_call_delta.id.clone(),
                name: None,
                arguments: String::new(),
                started: false,
                completed: false,
            });

            if state.provider_tool_call_id.is_none() {
                state.provider_tool_call_id = tool_call_delta.id.clone();
            }

            if let Some(name) = tool_call_delta
                .function
                .as_ref()
                .and_then(|function| function.name.as_ref())
                .filter(|name| !name.is_empty())
                && state.name.is_none()
            {
                state.name = Some(name.clone());
            }

            let metadata = metadata.with_provider_tool_call_id(state.provider_tool_call_id.clone());

            if !state.started {
                state.started = true;
                push_event(
                    events,
                    ids,
                    HarnessEvent::ToolCallStarted {
                        run_id: self.context.run_id.clone(),
                        tool_call_id: state.tool_call_id.clone(),
                        name: state.name.clone(),
                        metadata: metadata.clone(),
                    },
                );
            }

            if let Some(arguments_delta) = tool_call_delta
                .function
                .as_ref()
                .and_then(|function| function.arguments.as_ref())
                .filter(|arguments| !arguments.is_empty())
            {
                state.arguments.push_str(arguments_delta);
                push_event(
                    events,
                    ids,
                    HarnessEvent::ToolCallDelta {
                        run_id: self.context.run_id.clone(),
                        tool_call_id: state.tool_call_id.clone(),
                        arguments_delta: arguments_delta.clone(),
                        metadata,
                    },
                );
            }
        }
    }

    fn resolve_tool_call_key(&mut self, tool_call_delta: &ChatCompletionToolCallDelta) -> String {
        let aliases = tool_call_aliases(tool_call_delta);
        let key = aliases
            .iter()
            .find_map(|alias| self.tool_call_aliases.get(alias).cloned())
            .unwrap_or_else(|| aliases[0].clone());

        for alias in aliases {
            self.tool_call_aliases.insert(alias, key.clone());
        }

        key
    }

    fn complete_active_tool_calls(
        &mut self,
        metadata: &ProviderEventMetadata,
        ids: &mut impl HarnessEventIdSource,
        events: &mut Vec<HarnessEventEnvelope>,
    ) {
        for state in self.tool_calls.values_mut() {
            if state.completed {
                continue;
            }

            state.completed = true;
            push_event(
                events,
                ids,
                HarnessEvent::ToolCallCompleted {
                    run_id: self.context.run_id.clone(),
                    tool_call_id: state.tool_call_id.clone(),
                    name: state.name.clone(),
                    arguments: state.arguments.clone(),
                    metadata: metadata
                        .with_provider_tool_call_id(state.provider_tool_call_id.clone()),
                },
            );
        }
    }

    fn metadata_for_choice(
        &self,
        chunk: &ChatCompletionStreamChunk,
        choice: &ChatCompletionStreamChoice,
    ) -> ProviderEventMetadata {
        ProviderEventMetadata {
            provider_id: self.context.provider_id.clone(),
            configured_model_id: self.context.configured_model_id.clone(),
            provider_response_id: self.provider_response_id_for_chunk(chunk),
            provider_model: self.provider_model_for_chunk(chunk),
            choice_index: choice.index,
            provider_tool_call_id: None,
            usage: chunk.usage.as_ref().map(ProviderUsage::from),
        }
    }

    fn metadata_for_chunk(&self, chunk: &ChatCompletionStreamChunk) -> ProviderEventMetadata {
        ProviderEventMetadata {
            provider_id: self.context.provider_id.clone(),
            configured_model_id: self.context.configured_model_id.clone(),
            provider_response_id: self.provider_response_id_for_chunk(chunk),
            provider_model: self.provider_model_for_chunk(chunk),
            choice_index: None,
            provider_tool_call_id: None,
            usage: chunk.usage.as_ref().map(ProviderUsage::from),
        }
    }

    fn provider_response_id_for_chunk(&self, chunk: &ChatCompletionStreamChunk) -> Option<String> {
        chunk.id.clone().or_else(|| {
            self.last_metadata
                .as_ref()
                .and_then(|metadata| metadata.provider_response_id.clone())
        })
    }

    fn provider_model_for_chunk(&self, chunk: &ChatCompletionStreamChunk) -> Option<String> {
        chunk.model.clone().or_else(|| {
            self.last_metadata
                .as_ref()
                .and_then(|metadata| metadata.provider_model.clone())
        })
    }

    fn latest_metadata(&self) -> ProviderEventMetadata {
        self.last_metadata
            .clone()
            .unwrap_or_else(|| self.base_metadata())
    }

    fn base_metadata(&self) -> ProviderEventMetadata {
        ProviderEventMetadata {
            provider_id: self.context.provider_id.clone(),
            configured_model_id: self.context.configured_model_id.clone(),
            provider_response_id: None,
            provider_model: None,
            choice_index: None,
            provider_tool_call_id: None,
            usage: None,
        }
    }

    fn run_completed_event(&self) -> HarnessEvent {
        HarnessEvent::RunCompleted {
            run_id: self.context.run_id.clone(),
            metadata: self.latest_metadata(),
        }
    }

    fn provider_error_event(&self, error: OpenAiCompletionsError) -> HarnessEvent {
        let details = ProviderErrorDetails::from_openai_error(error);

        HarnessEvent::ProviderError {
            run_id: self.context.run_id.clone(),
            status: details.status,
            message: details.message,
            error_type: details.error_type,
            code: details.code,
            metadata: self.latest_metadata(),
        }
    }
}

struct ProviderErrorDetails {
    status: Option<u16>,
    message: String,
    error_type: Option<String>,
    code: Option<String>,
}

impl ProviderErrorDetails {
    fn from_openai_error(error: OpenAiCompletionsError) -> Self {
        match error {
            OpenAiCompletionsError::Provider(OpenAiCompletionsProviderError {
                status,
                message,
                error_type,
                code,
            }) => Self {
                status: Some(status),
                message,
                error_type,
                code,
            },
            OpenAiCompletionsError::ProviderStream(OpenAiCompletionsStreamProviderError {
                message,
                error_type,
                code,
            }) => Self {
                status: None,
                message,
                error_type,
                code,
            },
            OpenAiCompletionsError::Http { status, body } => Self {
                status: Some(status),
                message: body,
                error_type: None,
                code: None,
            },
            error => Self {
                status: None,
                message: error.to_string(),
                error_type: None,
                code: None,
            },
        }
    }
}

fn push_event(
    events: &mut Vec<HarnessEventEnvelope>,
    ids: &mut impl HarnessEventIdSource,
    event: HarnessEvent,
) {
    events.push(event_envelope(ids, event));
}

fn event_envelope(
    ids: &mut impl HarnessEventIdSource,
    event: HarnessEvent,
) -> HarnessEventEnvelope {
    HarnessEventEnvelope {
        event_id: ids.next_event_id(),
        event,
    }
}

fn tool_call_aliases(tool_call_delta: &ChatCompletionToolCallDelta) -> Vec<String> {
    let mut aliases = Vec::new();

    if let Some(index) = tool_call_delta.index {
        aliases.push(format!("index:{index}"));
    }

    if let Some(id) = tool_call_delta.id.as_ref().filter(|id| !id.is_empty()) {
        aliases.push(format!("id:{id}"));
    }

    if aliases.is_empty() {
        aliases.push("anonymous:0".to_string());
    }

    aliases
}
