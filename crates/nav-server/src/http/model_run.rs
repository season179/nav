use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nav_harness::events::ModelOutputContext;
use nav_harness::models::{
    ModelResolver, OpenAiCompletionsCancellationToken, OpenAiCompletionsClient,
    OpenAiCompletionsError, OpenAiCompletionsRequest, OpenAiCompletionsRequestContext,
    ResolveModelError, ResolvedModelConfig,
};
use nav_protocol::{BackendEvent, EventEnvelope, ProviderEventMetadata};
use nav_types::{EventId, MessageId, RunId, SessionId, ToolCallId};

use super::event_mapping::harness_events_to_backend_events;
use super::event_store::ProtocolEventStore;
use super::ids::ProtocolIdSource;
use super::{RunState, RunStatus};

#[derive(Debug, Clone, Default)]
pub(super) struct ModelRunService {
    client: OpenAiCompletionsClient,
}

#[derive(Debug)]
pub(super) struct ModelRunRequest<'a> {
    pub session_id: &'a SessionId,
    pub run_id: &'a RunId,
    pub message_id: &'a MessageId,
    pub text: &'a str,
}

impl ModelRunService {
    pub fn run_to_completion(
        &self,
        resolver: &ModelResolver,
        ids: Arc<Mutex<ProtocolIdSource>>,
        event_store: Arc<Mutex<ProtocolEventStore>>,
        runs: Arc<Mutex<HashMap<RunId, RunState>>>,
        cancellation_token: OpenAiCompletionsCancellationToken,
        request: ModelRunRequest<'_>,
    ) -> RunStatus {
        let model = match resolver.resolve_default() {
            Ok(model) => model,
            Err(error) => {
                publish_run_failure(
                    &ids,
                    &event_store,
                    &runs,
                    request.session_id,
                    request.run_id,
                    resolve_error_message(error),
                    Vec::new(),
                );
                return RunStatus::Failed;
            }
        };

        let completion_request = OpenAiCompletionsRequest::from_user(request.text);
        let request_context =
            OpenAiCompletionsRequestContext::new().with_cancellation_token(cancellation_token);
        let mut stream_ids = SharedProtocolIdSource {
            ids: Arc::clone(&ids),
        };
        let mut pending_provider_errors = Vec::new();
        let stream_result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("model streaming runtime should build")
            .block_on(self.client.stream_events_with_context(
                &model,
                &completion_request,
                &request_context,
                ModelOutputContext {
                    run_id: request.run_id.clone(),
                    message_id: request.message_id.clone(),
                    provider_id: model.provider_id.clone(),
                    configured_model_id: model.model.id.clone(),
                },
                &mut stream_ids,
                |harness_events| {
                    let (provider_errors, stream_events) = split_provider_errors(
                        harness_events_to_backend_events(request.session_id, harness_events),
                    );
                    pending_provider_errors.extend(provider_errors);
                    publish_stream_events(&event_store, &runs, request.run_id, stream_events);
                },
            ));

        match stream_result {
            Ok(()) => RunStatus::Completed,
            Err(OpenAiCompletionsError::Cancelled) => RunStatus::Cancelled,
            Err(error) => {
                if pending_provider_errors.is_empty()
                    && let Some(provider_error) = provider_error_event(
                        &ids,
                        request.session_id,
                        request.run_id,
                        &model,
                        &error,
                    )
                {
                    pending_provider_errors.push(provider_error);
                }
                publish_run_failure(
                    &ids,
                    &event_store,
                    &runs,
                    request.session_id,
                    request.run_id,
                    run_failed_message(&error),
                    pending_provider_errors,
                );
                RunStatus::Failed
            }
        }
    }
}

fn split_provider_errors(events: Vec<EventEnvelope>) -> (Vec<EventEnvelope>, Vec<EventEnvelope>) {
    let mut provider_errors = Vec::new();
    let mut stream_events = Vec::new();

    for event in events {
        if matches!(&event.event, BackendEvent::ProviderError { .. }) {
            provider_errors.push(event);
        } else {
            stream_events.push(event);
        }
    }

    (provider_errors, stream_events)
}

fn publish_stream_events(
    event_store: &Arc<Mutex<ProtocolEventStore>>,
    runs: &Arc<Mutex<HashMap<RunId, RunState>>>,
    run_id: &RunId,
    events: impl IntoIterator<Item = EventEnvelope>,
) {
    for event in events {
        if matches!(&event.event, BackendEvent::RunCompleted { .. }) {
            publish_terminal_event(event_store, runs, run_id, RunStatus::Completed, event);
        } else {
            publish_running_event(event_store, runs, run_id, event);
        }
    }
}

fn publish_run_failure(
    ids: &Arc<Mutex<ProtocolIdSource>>,
    event_store: &Arc<Mutex<ProtocolEventStore>>,
    runs: &Arc<Mutex<HashMap<RunId, RunState>>>,
    session_id: &SessionId,
    run_id: &RunId,
    message: String,
    provider_errors: Vec<EventEnvelope>,
) {
    let failed_event = run_failed_event(ids, session_id, run_id, message);
    publish_terminal_events(
        event_store,
        runs,
        run_id,
        RunStatus::Failed,
        provider_errors
            .into_iter()
            .chain(std::iter::once(failed_event)),
    );
}

fn publish_running_event(
    event_store: &Arc<Mutex<ProtocolEventStore>>,
    runs: &Arc<Mutex<HashMap<RunId, RunState>>>,
    run_id: &RunId,
    event: EventEnvelope,
) -> bool {
    let runs = runs.lock().unwrap();
    if !runs
        .get(run_id)
        .is_some_and(|run| run.status == RunStatus::Running)
    {
        return false;
    }

    event_store.lock().unwrap().append(event);
    true
}

fn publish_terminal_event(
    event_store: &Arc<Mutex<ProtocolEventStore>>,
    runs: &Arc<Mutex<HashMap<RunId, RunState>>>,
    run_id: &RunId,
    status: RunStatus,
    event: EventEnvelope,
) -> bool {
    publish_terminal_events(event_store, runs, run_id, status, std::iter::once(event))
}

fn publish_terminal_events(
    event_store: &Arc<Mutex<ProtocolEventStore>>,
    runs: &Arc<Mutex<HashMap<RunId, RunState>>>,
    run_id: &RunId,
    status: RunStatus,
    events: impl IntoIterator<Item = EventEnvelope>,
) -> bool {
    let mut runs = runs.lock().unwrap();
    let Some(run) = runs.get_mut(run_id) else {
        return false;
    };

    if run.status != RunStatus::Running {
        return false;
    }

    run.status = status;
    event_store.lock().unwrap().append_many(events);
    true
}

struct SharedProtocolIdSource {
    ids: Arc<Mutex<ProtocolIdSource>>,
}

impl nav_harness::events::HarnessEventIdSource for SharedProtocolIdSource {
    fn next_event_id(&mut self) -> EventId {
        self.ids.lock().unwrap().next_event_id()
    }

    fn next_tool_call_id(&mut self) -> ToolCallId {
        self.ids.lock().unwrap().next_tool_call_id()
    }
}

#[cfg(test)]
fn append_event(event_store: &Arc<Mutex<ProtocolEventStore>>, event: EventEnvelope) {
    event_store.lock().unwrap().append(event);
}

fn next_event_id(ids: &Arc<Mutex<ProtocolIdSource>>) -> EventId {
    ids.lock().unwrap().next_event_id()
}

fn resolve_error_message(error: ResolveModelError) -> String {
    format!("{error:?}")
}

fn run_failed_event(
    ids: &Arc<Mutex<ProtocolIdSource>>,
    session_id: &SessionId,
    run_id: &RunId,
    message: String,
) -> EventEnvelope {
    EventEnvelope {
        event_id: next_event_id(ids),
        session_id: session_id.clone(),
        event: BackendEvent::RunFailed {
            run_id: run_id.clone(),
            message,
        },
    }
}

fn provider_error_event(
    ids: &Arc<Mutex<ProtocolIdSource>>,
    session_id: &SessionId,
    run_id: &RunId,
    model: &ResolvedModelConfig,
    error: &OpenAiCompletionsError,
) -> Option<EventEnvelope> {
    let (status, message, error_type, code) = match error {
        OpenAiCompletionsError::Provider(error) => (
            Some(error.status),
            error.message.clone(),
            error.error_type.clone(),
            error.code.clone(),
        ),
        OpenAiCompletionsError::ProviderStream(error) => (
            None,
            error.message.clone(),
            error.error_type.clone(),
            error.code.clone(),
        ),
        OpenAiCompletionsError::MalformedResponse { message } => {
            (None, message.clone(), None, None)
        }
        _ => return None,
    };

    Some(EventEnvelope {
        event_id: next_event_id(ids),
        session_id: session_id.clone(),
        event: BackendEvent::ProviderError {
            run_id: run_id.clone(),
            status,
            message,
            error_type,
            code,
            metadata: ProviderEventMetadata {
                provider_id: model.provider_id.clone(),
                configured_model_id: model.model.id.clone(),
                provider_response_id: None,
                provider_model: None,
                choice_index: None,
                provider_tool_call_id: None,
                usage: None,
            },
        },
    })
}

fn run_failed_message(error: &OpenAiCompletionsError) -> String {
    match error {
        OpenAiCompletionsError::Provider(error) => {
            format!("provider error: {}", error.message)
        }
        OpenAiCompletionsError::ProviderStream(error) => {
            format!("provider error: {}", error.message)
        }
        error => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_events_are_not_published_after_run_stops_running() {
        let fixture = RunFixture::new(RunStatus::Cancelled);

        publish_stream_events(
            &fixture.event_store,
            &fixture.runs,
            &fixture.run_id,
            vec![
                fixture.text_delta_event("late delta"),
                fixture.run_completed_event(),
            ],
        );

        assert_eq!(fixture.event_types(), vec!["session.created"]);
        assert_eq!(fixture.run_status(), RunStatus::Cancelled);
    }

    #[test]
    fn terminal_stream_event_publishes_once_and_transitions_status() {
        let fixture = RunFixture::new(RunStatus::Running);

        publish_stream_events(
            &fixture.event_store,
            &fixture.runs,
            &fixture.run_id,
            vec![
                fixture.text_delta_event("hello"),
                fixture.run_completed_event(),
            ],
        );

        assert_eq!(
            fixture.event_types(),
            vec!["session.created", "model.text_delta", "run.completed"]
        );
        assert_eq!(fixture.run_status(), RunStatus::Completed);
    }

    #[test]
    fn failed_run_events_are_not_published_after_run_stops_running() {
        let fixture = RunFixture::new(RunStatus::Cancelled);
        let ids = fixture.shared_ids();

        publish_run_failure(
            &ids,
            &fixture.event_store,
            &fixture.runs,
            &fixture.session_id,
            &fixture.run_id,
            "provider stopped".to_string(),
            Vec::new(),
        );

        assert_eq!(fixture.event_types(), vec!["session.created"]);
        assert_eq!(fixture.run_status(), RunStatus::Cancelled);
    }

    struct RunFixture {
        ids: Mutex<ProtocolIdSource>,
        event_store: Arc<Mutex<ProtocolEventStore>>,
        runs: Arc<Mutex<HashMap<RunId, RunState>>>,
        session_id: SessionId,
        run_id: RunId,
        message_id: MessageId,
    }

    impl RunFixture {
        fn new(status: RunStatus) -> Self {
            let mut ids = ProtocolIdSource::default();
            let session_id = ids.next_session_id();
            let run_id = ids.next_run_id();
            let message_id = ids.next_message_id();
            let event_store = Arc::new(Mutex::new(ProtocolEventStore::default()));
            let runs = Arc::new(Mutex::new(HashMap::from([(
                run_id.clone(),
                RunState {
                    session_id: session_id.clone(),
                    status,
                    cancellation_token: None,
                },
            )])));

            append_event(
                &event_store,
                EventEnvelope {
                    event_id: ids.next_event_id(),
                    session_id: session_id.clone(),
                    event: BackendEvent::SessionCreated,
                },
            );

            Self {
                ids: Mutex::new(ids),
                event_store,
                runs,
                session_id,
                run_id,
                message_id,
            }
        }

        fn shared_ids(&self) -> Arc<Mutex<ProtocolIdSource>> {
            Arc::new(Mutex::new(self.ids.lock().unwrap().clone()))
        }

        fn text_delta_event(&self, delta: &str) -> EventEnvelope {
            EventEnvelope {
                event_id: self.next_event_id(),
                session_id: self.session_id.clone(),
                event: BackendEvent::ModelTextDelta {
                    run_id: self.run_id.clone(),
                    message_id: self.message_id.clone(),
                    delta: delta.to_string(),
                    metadata: provider_metadata(),
                },
            }
        }

        fn run_completed_event(&self) -> EventEnvelope {
            EventEnvelope {
                event_id: self.next_event_id(),
                session_id: self.session_id.clone(),
                event: BackendEvent::RunCompleted {
                    run_id: self.run_id.clone(),
                    metadata: Some(provider_metadata()),
                },
            }
        }

        fn next_event_id(&self) -> EventId {
            self.ids.lock().unwrap().next_event_id()
        }

        fn event_types(&self) -> Vec<&'static str> {
            self.event_store
                .lock()
                .unwrap()
                .replay_after(&self.session_id, None)
                .unwrap()
                .iter()
                .map(|event| event.event.event_type())
                .collect()
        }

        fn run_status(&self) -> RunStatus {
            self.runs.lock().unwrap().get(&self.run_id).unwrap().status
        }
    }

    fn provider_metadata() -> ProviderEventMetadata {
        ProviderEventMetadata {
            provider_id: "compatible-gateway".to_string(),
            configured_model_id: "vendor/model-large".to_string(),
            provider_response_id: None,
            provider_model: None,
            choice_index: None,
            provider_tool_call_id: None,
            usage: None,
        }
    }
}
