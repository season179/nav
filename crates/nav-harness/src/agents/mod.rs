//! Agent roles, loops, delegation, task state, and autonomy limits.

use nav_types::{MessageId, RunId};

use crate::events::{
    HarnessEvent, HarnessEventEnvelope, HarnessEventIdSource, ModelOutputContext,
    ProviderEventMetadata,
};
use crate::guardrails::{GuardrailError, ToolCallContext, ToolCallContextParams};
use crate::models::{
    OpenAiCompletionsCancellationToken, OpenAiCompletionsClient, OpenAiCompletionsError,
    OpenAiCompletionsRequest, OpenAiCompletionsRequestContext, ResolvedModelConfig,
};
use crate::sessions::{ToolCall, Turn};
use crate::tools::{ToolCancellationToken, ToolContext, ToolPreset, ToolRegistry};

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
    pub tool_registry: &'a ToolRegistry,
    pub tool_preset: ToolPreset,
    pub tool_context: &'a ToolContext,
    pub cancellation_token: OpenAiCompletionsCancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunLoopCompletion {
    pub turns: Vec<Turn>,
    pub terminal_events: Vec<HarnessEventEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunLoopResult {
    Completed(RunLoopCompletion),
    Cancelled,
    Failed(OpenAiCompletionsError),
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
        let request_context = OpenAiCompletionsRequestContext::new()
            .with_cancellation_token(request.cancellation_token.clone());
        let output_context = ModelOutputContext {
            run_id: request.run_id.clone(),
            message_id: request.message_id.clone(),
            provider_id: model.provider_id.clone(),
            configured_model_id: model.model.id.clone(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("model streaming runtime should build");
        let mut turns = request.turns.to_vec();
        let mut new_turns = Vec::new();

        loop {
            let completion_request = OpenAiCompletionsRequest::from_turns_with_tools(
                &turns,
                request.tool_registry,
                request.tool_preset,
            );
            let model_turn = match self.stream_model_turn(StreamModelTurnRequest {
                runtime: &runtime,
                model,
                completion_request: &completion_request,
                request_context: &request_context,
                output_context: &output_context,
                ids,
                emit: &mut emit,
            }) {
                Ok(model_turn) => model_turn,
                Err(OpenAiCompletionsError::Cancelled) => return RunLoopResult::Cancelled,
                Err(error) => return RunLoopResult::Failed(error),
            };

            let tool_calls = model_turn.tool_calls;
            if let Some(turn) = model_turn.assistant_turn {
                turns.push(turn.clone());
                new_turns.push(turn);
            }

            if tool_calls.is_empty() {
                return RunLoopResult::Completed(RunLoopCompletion {
                    turns: new_turns,
                    terminal_events: model_turn.terminal_events,
                });
            }

            let tool_cancel = ToolCancellationToken::new();
            if request.cancellation_token.is_cancelled() {
                tool_cancel.cancel();
            }
            let tool_dispatch_metadata = ProviderEventMetadata {
                provider_id: output_context.provider_id.clone(),
                configured_model_id: output_context.configured_model_id.clone(),
                provider_response_id: None,
                provider_model: None,
                choice_index: None,
                provider_tool_call_id: None,
                usage: None,
            };
            match dispatch_tool_calls(ToolDispatchRequest {
                tool_calls: &tool_calls,
                registry: request.tool_registry,
                tool_preset: request.tool_preset,
                context: request.tool_context,
                cancel: tool_cancel,
                run_cancel: Some(request.cancellation_token.clone()),
                run_id: request.run_id,
                ids,
                emit: &mut emit,
                base_metadata: &tool_dispatch_metadata,
            }) {
                ToolDispatchResult::Completed(tool_turns) => {
                    turns.extend(tool_turns.clone());
                    new_turns.extend(tool_turns);
                }
                ToolDispatchResult::Cancelled => return RunLoopResult::Cancelled,
            }
        }
    }

    fn stream_model_turn<Ids, Emit>(
        &self,
        request: StreamModelTurnRequest<'_, Ids, Emit>,
    ) -> Result<ModelTurnOutput, OpenAiCompletionsError>
    where
        Ids: HarnessEventIdSource,
        Emit: FnMut(Vec<HarnessEventEnvelope>),
    {
        let StreamModelTurnRequest {
            runtime,
            model,
            completion_request,
            request_context,
            output_context,
            ids,
            emit,
        } = request;
        let mut capture = AssistantTurnCapture::default();
        let mut terminal_events = Vec::new();

        runtime.block_on(self.client.stream_events_with_context(
            model,
            completion_request,
            request_context,
            output_context.clone(),
            ids,
            |harness_events| {
                capture.observe(&harness_events);
                let (stream_events, completed_events) = split_run_completion_events(harness_events);
                terminal_events.extend(completed_events);
                if !stream_events.is_empty() {
                    (emit)(stream_events);
                }
            },
        ))?;

        let tool_calls = capture.tool_calls.clone();
        Ok(ModelTurnOutput {
            assistant_turn: capture.into_turn(),
            tool_calls,
            terminal_events,
        })
    }
}

struct StreamModelTurnRequest<'a, Ids, Emit>
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    runtime: &'a tokio::runtime::Runtime,
    model: &'a ResolvedModelConfig,
    completion_request: &'a OpenAiCompletionsRequest,
    request_context: &'a OpenAiCompletionsRequestContext,
    output_context: &'a ModelOutputContext,
    ids: &'a mut Ids,
    emit: &'a mut Emit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelTurnOutput {
    assistant_turn: Option<Turn>,
    tool_calls: Vec<ToolCall>,
    terminal_events: Vec<HarnessEventEnvelope>,
}

#[derive(Debug, Default)]
struct AssistantTurnCapture {
    text: String,
    tool_calls: Vec<ToolCall>,
}

impl AssistantTurnCapture {
    fn observe(&mut self, events: &[HarnessEventEnvelope]) {
        for event in events {
            match &event.event {
                HarnessEvent::ModelTextDelta { delta, .. } => self.text.push_str(delta),
                HarnessEvent::ToolCallStarted { .. } | HarnessEvent::ToolCallDelta { .. } => {}
                HarnessEvent::ToolCallCompleted {
                    tool_call_id,
                    name,
                    arguments,
                    metadata,
                    ..
                } => self.tool_calls.push(ToolCall {
                    id: metadata
                        .provider_tool_call_id
                        .clone()
                        .unwrap_or_else(|| tool_call_id.to_string()),
                    tool_call_id: Some(tool_call_id.clone()),
                    name: name.clone().unwrap_or_default(),
                    arguments: arguments.clone(),
                }),
                _ => {}
            }
        }
    }

    fn into_turn(self) -> Option<Turn> {
        if self.tool_calls.is_empty() {
            return (!self.text.is_empty()).then(|| Turn::assistant_text(self.text));
        }

        if self.text.is_empty() {
            Some(Turn::assistant_tool_calls(self.tool_calls))
        } else {
            Some(Turn::assistant_text_with_tool_calls(
                self.text,
                self.tool_calls,
            ))
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolDispatchResult {
    Completed(Vec<Turn>),
    Cancelled,
}

struct ToolDispatchRequest<'a, Ids, Emit>
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    tool_calls: &'a [ToolCall],
    registry: &'a ToolRegistry,
    tool_preset: ToolPreset,
    context: &'a ToolContext,
    cancel: ToolCancellationToken,
    run_cancel: Option<OpenAiCompletionsCancellationToken>,
    run_id: &'a RunId,
    ids: &'a mut Ids,
    emit: &'a mut Emit,
    base_metadata: &'a ProviderEventMetadata,
}

fn dispatch_tool_calls<Ids, Emit>(request: ToolDispatchRequest<'_, Ids, Emit>) -> ToolDispatchResult
where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let ToolDispatchRequest {
        tool_calls,
        registry,
        tool_preset,
        context,
        cancel,
        run_cancel,
        run_id,
        ids,
        emit,
        base_metadata,
    } = request;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tool dispatch runtime should build");
    if let Some(run_cancel) = run_cancel {
        let tool_cancel = cancel.clone();
        runtime.spawn(async move {
            run_cancel.cancelled().await;
            tool_cancel.cancel();
        });
    }
    let mut turns = Vec::new();

    for tool_call in tool_calls {
        if cancel.is_cancelled() {
            return ToolDispatchResult::Cancelled;
        }

        let Some(tool) = registry.get(&tool_call.name) else {
            let message = format!("unknown tool `{}`", tool_call.name);
            emit_tool_call_failed(ids, emit, run_id, tool_call, &message, base_metadata);
            turns.push(tool_error_turn(tool_call, message));
            continue;
        };

        let args: serde_json::Value = match serde_json::from_str(&tool_call.arguments) {
            Ok(args) => args,
            Err(error) => {
                let message = format!("tool call arguments are not valid JSON: {error}");
                emit_tool_call_failed(ids, emit, run_id, tool_call, &message, base_metadata);
                turns.push(tool_error_turn(tool_call, message));
                continue;
            }
        };

        let guardrail_context = ToolCallContext::new(ToolCallContextParams {
            tool_name: &tool_call.name,
            raw_arguments: tool_call.arguments.clone(),
            parsed_arguments: args.clone(),
            preset: tool_preset,
            risk_class: tool.risk_class(),
            tool_context: context,
            call_id: &tool_call.id,
            nav_tool_call_id: tool_call.tool_call_id.clone(),
            run_id: run_id.clone(),
        });

        if let Err(error) = context.guardrails().before_tool_call(&guardrail_context) {
            let message = error.message();
            if let GuardrailError::ConfirmationRequired { reason, .. } = &error {
                emit_tool_approval_requested(
                    ids,
                    emit,
                    run_id,
                    tool_call,
                    reason,
                    &guardrail_context.arguments.summary,
                    tool.risk_class().name(),
                );
            }
            emit_tool_call_failed(ids, emit, run_id, tool_call, &message, base_metadata);
            turns.push(tool_error_turn(tool_call, message));
            continue;
        }

        let result = runtime.block_on(tool.execute(context, args, cancel.clone()));
        if cancel.is_cancelled() {
            return ToolDispatchResult::Cancelled;
        }

        match result {
            Ok(output) => match context
                .guardrails()
                .after_tool_call(&guardrail_context, output)
            {
                Ok(output) => turns.push(Turn::tool_result(&tool_call.id, output.content)),
                Err(error) => {
                    let message = error.message();
                    emit_tool_call_failed(ids, emit, run_id, tool_call, &message, base_metadata);
                    turns.push(tool_error_turn(tool_call, message));
                }
            },
            Err(error) => {
                let message = error.message();
                emit_tool_call_failed(ids, emit, run_id, tool_call, message, base_metadata);
                turns.push(tool_error_turn(tool_call, message));
            }
        }
    }

    ToolDispatchResult::Completed(turns)
}

fn tool_error_turn(tool_call: &ToolCall, message: impl Into<String>) -> Turn {
    Turn::tool_result(&tool_call.id, structured_tool_error(message))
}

fn emit_tool_call_failed<Ids, Emit>(
    ids: &mut Ids,
    emit: &mut Emit,
    run_id: &RunId,
    tool_call: &ToolCall,
    message: &str,
    base_metadata: &ProviderEventMetadata,
) where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let Some(nav_tool_call_id) = &tool_call.tool_call_id else {
        return;
    };

    let event_id = ids.next_event_id();
    let event = HarnessEventEnvelope {
        event_id,
        event: HarnessEvent::ToolCallFailed {
            run_id: run_id.clone(),
            tool_call_id: nav_tool_call_id.clone(),
            name: Some(tool_call.name.clone()),
            error_message: message.to_string(),
            metadata: base_metadata.clone(),
        },
    };
    emit(vec![event]);
}

fn emit_tool_approval_requested<Ids, Emit>(
    ids: &mut Ids,
    emit: &mut Emit,
    run_id: &RunId,
    tool_call: &ToolCall,
    reason: &str,
    arguments_summary: &str,
    risk_class: &str,
) where
    Ids: HarnessEventIdSource,
    Emit: FnMut(Vec<HarnessEventEnvelope>),
{
    let Some(nav_tool_call_id) = &tool_call.tool_call_id else {
        return;
    };

    let event = HarnessEventEnvelope {
        event_id: ids.next_event_id(),
        event: HarnessEvent::ToolApprovalRequested {
            run_id: run_id.clone(),
            tool_call_id: nav_tool_call_id.clone(),
            approval_id: ids.next_approval_id(),
            tool_name: tool_call.name.clone(),
            reason: reason.to_string(),
            arguments_summary: arguments_summary.to_string(),
            risk_class: Some(risk_class.to_string()),
        },
    };
    emit(vec![event]);
}

fn structured_tool_error(message: impl Into<String>) -> String {
    serde_json::json!({
        "ok": false,
        "error": {
            "message": message.into(),
        },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    use nav_types::{ApprovalId, EventId, ToolCallId as NavToolCallId};
    use serde_json::{Value, json};

    use super::*;
    use crate::events::HarnessEventIdSource;
    use crate::guardrails::{
        BeforeToolCallDecision, ConfirmationPolicy, GuardrailError, GuardrailRunner,
        ToolCallContext, ToolGuardrailHook,
    };
    use crate::sessions::ToolCall;
    use crate::tools::{
        NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolFuture, ToolOutput,
        ToolRegistry, read,
    };
    use crate::workspace::path::WorkspacePathPolicy;

    struct TestIdSource;

    impl HarnessEventIdSource for TestIdSource {
        fn next_event_id(&mut self) -> EventId {
            EventId::try_new("019f2f6f-f178-7a72-9f28-000000000099").unwrap()
        }

        fn next_tool_call_id(&mut self) -> NavToolCallId {
            NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000098").unwrap()
        }

        fn next_approval_id(&mut self) -> ApprovalId {
            ApprovalId::try_new("019f2f6f-f178-7a72-9f28-000000000040").unwrap()
        }
    }

    fn test_metadata() -> ProviderEventMetadata {
        ProviderEventMetadata {
            provider_id: "test-provider".to_string(),
            configured_model_id: "test-model".to_string(),
            provider_response_id: None,
            provider_model: None,
            choice_index: None,
            provider_tool_call_id: None,
            usage: None,
        }
    }

    fn dispatch_test(
        tool_calls: &[ToolCall],
        registry: &ToolRegistry,
        context: &ToolContext,
        cancel: ToolCancellationToken,
        run_cancel: Option<OpenAiCompletionsCancellationToken>,
    ) -> ToolDispatchResult {
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();
        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls,
            registry,
            tool_preset: ToolPreset::Coding,
            context,
            cancel,
            run_cancel,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });
        let _ = events;
        result
    }

    #[test]
    fn dispatches_single_tool_call_success_as_tool_turn() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let tool_calls = vec![ToolCall {
            id: "call_echo_1".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: r#"{"text":"hello"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            None,
        );

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![Turn::tool_result("call_echo_1", "hello")])
        );
    }

    #[test]
    fn dispatches_multiple_tool_call_successes_in_order() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let tool_calls = vec![
            ToolCall {
                id: "call_echo_1".to_string(),
                tool_call_id: None,
                name: "echo".to_string(),
                arguments: r#"{"text":"first"}"#.to_string(),
            },
            ToolCall {
                id: "call_echo_2".to_string(),
                tool_call_id: None,
                name: "echo".to_string(),
                arguments: r#"{"text":"second"}"#.to_string(),
            },
        ];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            None,
        );

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![
                Turn::tool_result("call_echo_1", "first"),
                Turn::tool_result("call_echo_2", "second"),
            ])
        );
    }

    #[test]
    fn dispatch_returns_structured_error_turn_for_bad_tool_args() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let tool_calls = vec![ToolCall {
            id: "call_echo_bad".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: "not json".to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("bad args should complete with an error tool turn");
        };
        assert_eq!(turns.len(), 1);
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not valid JSON")
        );
    }

    #[test]
    fn dispatch_returns_structured_error_turn_for_unknown_tool() {
        let registry = ToolRegistry::default();
        let tool_calls = vec![ToolCall {
            id: "call_missing".to_string(),
            tool_call_id: None,
            name: "missing".to_string(),
            arguments: "{}".to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("unknown tool should complete with an error tool turn");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["message"], "unknown tool `missing`");
    }

    #[test]
    fn dispatch_honors_cancellation_mid_execute() {
        let mut registry = ToolRegistry::default();
        registry
            .register(WaitForCancelTool)
            .expect("wait should register");
        let tool_calls = vec![ToolCall {
            id: "call_wait".to_string(),
            tool_call_id: None,
            name: "wait".to_string(),
            arguments: "{}".to_string(),
        }];
        let cancel = ToolCancellationToken::new();
        let cancel_from_thread = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel_from_thread.cancel();
        });

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            cancel,
            None,
        );
        assert_eq!(result, ToolDispatchResult::Cancelled);
    }

    #[test]
    fn dispatch_bridges_run_cancellation_to_tool_token() {
        let mut registry = ToolRegistry::default();
        registry
            .register(WaitForCancelTool)
            .expect("wait should register");
        let tool_calls = vec![ToolCall {
            id: "call_wait".to_string(),
            tool_call_id: None,
            name: "wait".to_string(),
            arguments: "{}".to_string(),
        }];
        let run_cancel = OpenAiCompletionsCancellationToken::new();
        let cancel_from_thread = run_cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel_from_thread.cancel();
        });

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &ToolContext::default(),
            ToolCancellationToken::new(),
            Some(run_cancel),
        );
        assert_eq!(result, ToolDispatchResult::Cancelled);
    }

    #[test]
    fn dispatch_returns_structured_error_for_read_path_escape() {
        let workspace = TestWorkspace::new("read_escape");
        let mut registry = ToolRegistry::default();
        read::register(&mut registry).expect("read should register");
        let context = ToolContext::with_path_policy(workspace.policy());
        let tool_calls = vec![ToolCall {
            id: "call_read_escape".to_string(),
            tool_call_id: None,
            name: "read".to_string(),
            arguments: r#"{"path":"../secret.txt"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("path policy rejection should be returned as a tool error");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("escapes workspace")
        );
    }

    #[test]
    fn dispatch_returns_structured_error_when_guardrail_denies_tool_call() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(DenyGuardrailHook)
            .expect("deny hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_echo_guarded".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: r#"{"text":"hello"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("guardrail denial should complete with an error tool turn");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]["message"]
                .as_str()
                .expect("message should be a string")
                .contains("blocked by test guardrail")
        );
    }

    #[test]
    fn dispatch_fails_closed_when_guardrail_requests_confirmation() {
        let mut registry = ToolRegistry::default();
        registry
            .register(PanicIfExecutedTool)
            .expect("panic tool should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(ConfirmGuardrailHook)
            .expect("confirmation hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_confirm".to_string(),
            tool_call_id: None,
            name: "panic-if-executed".to_string(),
            arguments: "{}".to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        let ToolDispatchResult::Completed(turns) = result else {
            panic!("confirmation request should complete with an error tool turn");
        };
        let error: Value = serde_json::from_str(&turns[0].text_content())
            .expect("tool error should be structured JSON");
        assert_eq!(error["ok"], false);
        assert!(
            error["error"]["message"]
                .as_str()
                .expect("message should be a string")
                .contains("no approval channel is available")
        );
    }

    #[test]
    fn dispatch_emits_approval_requested_when_guardrail_requests_confirmation() {
        let mut registry = ToolRegistry::default();
        registry
            .register(PanicIfExecutedTool)
            .expect("panic tool should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(ConfirmGuardrailHook)
            .expect("confirmation hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let nav_id = NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
        let tool_calls = vec![ToolCall {
            id: "call_confirm".to_string(),
            tool_call_id: Some(nav_id.clone()),
            name: "panic-if-executed".to_string(),
            arguments: r#"{"path":"notes.md","content":"hello"}"#.to_string(),
        }];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &context,
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert!(matches!(result, ToolDispatchResult::Completed(_)));
        assert_eq!(events.len(), 2);
        match &events[0].event {
            HarnessEvent::ToolApprovalRequested {
                run_id: rid,
                tool_call_id,
                tool_name,
                reason,
                arguments_summary,
                risk_class,
                ..
            } => {
                assert_eq!(rid, &run_id);
                assert_eq!(tool_call_id, &nav_id);
                assert_eq!(tool_name, "panic-if-executed");
                assert_eq!(reason, "tool requires approval");
                assert_eq!(
                    arguments_summary,
                    r#"{"content":"hello","path":"notes.md"}"#
                );
                assert_eq!(risk_class.as_deref(), Some("exec"));
            }
            other => panic!("expected ToolApprovalRequested, got {other:?}"),
        }
        assert!(matches!(
            events[1].event,
            HarnessEvent::ToolCallFailed { .. }
        ));
    }

    #[test]
    fn dispatch_executes_confirmation_request_with_scripted_approval() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let mut guardrails =
            GuardrailRunner::default().with_confirmation_policy(ConfirmationPolicy::ScriptedAllow);
        guardrails
            .register_hook(ConfirmGuardrailHook)
            .expect("confirmation hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_confirm_approved".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: r#"{"text":"approved"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![Turn::tool_result(
                "call_confirm_approved",
                "approved",
            )])
        );
    }

    #[test]
    fn dispatch_applies_after_guardrails_to_successful_tool_output() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let mut guardrails = GuardrailRunner::default();
        guardrails
            .register_hook(RedactAfterGuardrailHook)
            .expect("redaction hook should register");
        let context = ToolContext::default().with_guardrails(guardrails);
        let tool_calls = vec![ToolCall {
            id: "call_echo_secret".to_string(),
            tool_call_id: None,
            name: "echo".to_string(),
            arguments: r#"{"text":"token=secret"}"#.to_string(),
        }];

        let result = dispatch_test(
            &tool_calls,
            &registry,
            &context,
            ToolCancellationToken::new(),
            None,
        );

        assert_eq!(
            result,
            ToolDispatchResult::Completed(vec![Turn::tool_result(
                "call_echo_secret",
                "token=[redacted]",
            )])
        );
    }

    #[test]
    fn dispatch_emits_tool_call_failed_event_for_unknown_tool() {
        let registry = ToolRegistry::default();
        let nav_id = NavToolCallId::try_new("019f2f6f-f178-7a72-9f28-000000000050").unwrap();
        let tool_calls = vec![ToolCall {
            id: "call_missing".to_string(),
            tool_call_id: Some(nav_id.clone()),
            name: "missing".to_string(),
            arguments: "{}".to_string(),
        }];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &ToolContext::default(),
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert!(matches!(result, ToolDispatchResult::Completed(_)));
        assert_eq!(events.len(), 1);
        match &events[0].event {
            HarnessEvent::ToolCallFailed {
                run_id: rid,
                tool_call_id: tcid,
                name,
                error_message,
                ..
            } => {
                assert_eq!(rid, &run_id);
                assert_eq!(tcid, &nav_id);
                assert_eq!(name.as_deref(), Some("missing"));
                assert!(error_message.contains("unknown tool"));
            }
            other => panic!("expected ToolCallFailed, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_does_not_emit_tool_call_failed_when_no_nav_tool_call_id() {
        let registry = ToolRegistry::default();
        let tool_calls = vec![ToolCall {
            id: "call_missing".to_string(),
            tool_call_id: None,
            name: "missing".to_string(),
            arguments: "{}".to_string(),
        }];
        let run_id = RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001").unwrap();
        let mut ids = TestIdSource;
        let mut events: Vec<HarnessEventEnvelope> = Vec::new();
        let metadata = test_metadata();

        let _result = dispatch_tool_calls(ToolDispatchRequest {
            tool_calls: &tool_calls,
            registry: &registry,
            tool_preset: ToolPreset::Coding,
            context: &ToolContext::default(),
            cancel: ToolCancellationToken::new(),
            run_cancel: None,
            run_id: &run_id,
            ids: &mut ids,
            emit: &mut |envelopes| events.extend(envelopes),
            base_metadata: &metadata,
        });

        assert!(
            events.is_empty(),
            "no events should be emitted without a nav tool_call_id"
        );
    }

    struct EchoTool;

    impl NavTool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes text."
        }
        fn parameters(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
                "additionalProperties": false
            })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Read
        }
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            args: Value,
            _cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                Ok(ToolOutput::text(
                    args["text"].as_str().unwrap_or_default().to_string(),
                ))
            })
        }
    }

    struct WaitForCancelTool;

    impl NavTool for WaitForCancelTool {
        fn name(&self) -> &str {
            "wait"
        }
        fn description(&self) -> &str {
            "Waits for cancellation."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Read
        }
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            _args: Value,
            cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                cancel.cancelled().await;
                Ok(ToolOutput::text("late output"))
            })
        }
    }

    struct PanicIfExecutedTool;

    impl NavTool for PanicIfExecutedTool {
        fn name(&self) -> &str {
            "panic-if-executed"
        }
        fn description(&self) -> &str {
            "Panics if executed."
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object", "properties": {}, "additionalProperties": false })
        }
        fn risk_class(&self) -> RiskClass {
            RiskClass::Exec
        }
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolContext,
            _args: Value,
            _cancel: ToolCancellationToken,
        ) -> ToolFuture<'a> {
            panic!("confirmation should stop execution before this tool runs")
        }
    }

    #[derive(Debug)]
    struct DenyGuardrailHook;

    impl ToolGuardrailHook for DenyGuardrailHook {
        fn name(&self) -> &str {
            "deny-test"
        }

        fn before_tool_call(
            &self,
            _context: &ToolCallContext,
        ) -> Result<BeforeToolCallDecision, GuardrailError> {
            Ok(BeforeToolCallDecision::Deny {
                reason: "blocked by test guardrail".to_string(),
            })
        }
    }

    #[derive(Debug)]
    struct ConfirmGuardrailHook;

    impl ToolGuardrailHook for ConfirmGuardrailHook {
        fn name(&self) -> &str {
            "confirm-test"
        }

        fn before_tool_call(
            &self,
            _context: &ToolCallContext,
        ) -> Result<BeforeToolCallDecision, GuardrailError> {
            Ok(BeforeToolCallDecision::RequestConfirmation {
                reason: "tool requires approval".to_string(),
                summary: "Confirm test tool call".to_string(),
            })
        }
    }

    #[derive(Debug)]
    struct RedactAfterGuardrailHook;

    impl ToolGuardrailHook for RedactAfterGuardrailHook {
        fn name(&self) -> &str {
            "redact-after-test"
        }

        fn after_tool_call(
            &self,
            context: &ToolCallContext,
            output: ToolOutput,
        ) -> Result<ToolOutput, GuardrailError> {
            assert_eq!(context.tool_name, "echo");
            assert_eq!(context.arguments.parsed["text"], "token=secret");
            Ok(ToolOutput::text(
                output.content.replace("secret", "[redacted]"),
            ))
        }
    }

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("nav-agents-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(root).expect("workspace should canonicalize"),
            }
        }
        fn policy(&self) -> WorkspacePathPolicy {
            WorkspacePathPolicy::new(&self.root, &self.root)
                .expect("path policy should accept workspace")
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
