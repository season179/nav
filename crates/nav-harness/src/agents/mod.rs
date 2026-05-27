//! Agent roles, loops, delegation, task state, and autonomy limits.

use nav_types::{MessageId, RunId};

use crate::events::{HarnessEvent, HarnessEventEnvelope, HarnessEventIdSource, ModelOutputContext};
use crate::models::{
    OpenAiCompletionsCancellationToken, OpenAiCompletionsClient, OpenAiCompletionsError,
    OpenAiCompletionsRequest, OpenAiCompletionsRequestContext, ResolvedModelConfig,
};
use crate::sessions::Turn;

#[derive(Debug, Default)]
pub struct AgentCatalog;

#[derive(Debug, Clone, Default)]
pub struct RunLoop {
    client: OpenAiCompletionsClient,
}

#[derive(Debug)]
pub struct RunLoopRequest<'a> {
    pub run_id: &'a RunId,
    pub message_id: &'a MessageId,
    pub turns: &'a [Turn],
    pub cancellation_token: OpenAiCompletionsCancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLoopCompletion {
    pub assistant_turn: Option<Turn>,
    pub terminal_events: Vec<HarnessEventEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunLoopFailure {
    Model(OpenAiCompletionsError),
    ToolExecutionNotImplemented,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunLoopResult {
    Completed(RunLoopCompletion),
    Cancelled,
    Failed(RunLoopFailure),
}

impl RunLoop {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_client(client: OpenAiCompletionsClient) -> Self {
        Self { client }
    }

    pub fn run(
        &self,
        model: &ResolvedModelConfig,
        request: RunLoopRequest<'_>,
        ids: &mut impl HarnessEventIdSource,
        mut emit: impl FnMut(Vec<HarnessEventEnvelope>),
    ) -> RunLoopResult {
        let completion_request = OpenAiCompletionsRequest::from_turns(request.turns);
        let request_context = OpenAiCompletionsRequestContext::new()
            .with_cancellation_token(request.cancellation_token);
        let output_context = ModelOutputContext {
            run_id: request.run_id.clone(),
            message_id: request.message_id.clone(),
            provider_id: model.provider_id.clone(),
            configured_model_id: model.model.id.clone(),
        };
        let mut assistant_turn = AssistantTurnCapture::default();
        let mut terminal_events = Vec::new();

        let stream_result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("model streaming runtime should build")
            .block_on(self.client.stream_events_with_context(
                model,
                &completion_request,
                &request_context,
                output_context,
                ids,
                |harness_events| {
                    assistant_turn.observe(&harness_events);
                    let (stream_events, completed_events) =
                        split_run_completion_events(harness_events);
                    terminal_events.extend(completed_events);
                    if !stream_events.is_empty() {
                        emit(stream_events);
                    }
                },
            ));

        match stream_result {
            Ok(()) if assistant_turn.has_tool_calls => {
                RunLoopResult::Failed(RunLoopFailure::ToolExecutionNotImplemented)
            }
            Ok(()) => RunLoopResult::Completed(RunLoopCompletion {
                assistant_turn: assistant_turn.into_turn(),
                terminal_events,
            }),
            Err(OpenAiCompletionsError::Cancelled) => RunLoopResult::Cancelled,
            Err(error) => RunLoopResult::Failed(RunLoopFailure::Model(error)),
        }
    }
}

#[derive(Debug, Default)]
struct AssistantTurnCapture {
    text: String,
    has_tool_calls: bool,
}

impl AssistantTurnCapture {
    fn observe(&mut self, events: &[HarnessEventEnvelope]) {
        for event in events {
            match &event.event {
                HarnessEvent::ModelTextDelta { delta, .. } => self.text.push_str(delta),
                HarnessEvent::ToolCallStarted { .. }
                | HarnessEvent::ToolCallDelta { .. }
                | HarnessEvent::ToolCallCompleted { .. } => {
                    self.has_tool_calls = true;
                }
                HarnessEvent::MessageCompleted { finish_reason, .. }
                    if finish_reason.as_deref() == Some("tool_calls") =>
                {
                    self.has_tool_calls = true;
                }
                _ => {}
            }
        }
    }

    fn into_turn(self) -> Option<Turn> {
        (!self.text.is_empty()).then(|| Turn::assistant_text(self.text))
    }
}

fn split_run_completion_events(
    events: Vec<HarnessEventEnvelope>,
) -> (Vec<HarnessEventEnvelope>, Vec<HarnessEventEnvelope>) {
    let mut stream_events = Vec::new();
    let mut completed_events = Vec::new();

    for event in events {
        if matches!(event.event, HarnessEvent::RunCompleted { .. }) {
            completed_events.push(event);
        } else {
            stream_events.push(event);
        }
    }

    (stream_events, completed_events)
}
