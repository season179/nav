//! Agent roles, loops, delegation, task state, and autonomy limits.

use nav_types::{MessageId, RunId};

use crate::events::{HarnessEvent, HarnessEventEnvelope, HarnessEventIdSource, ModelOutputContext};
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
            match dispatch_tool_calls(
                &tool_calls,
                request.tool_registry,
                request.tool_context,
                tool_cancel,
                Some(request.cancellation_token.clone()),
            ) {
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

fn dispatch_tool_calls(
    tool_calls: &[ToolCall],
    registry: &ToolRegistry,
    context: &ToolContext,
    cancel: ToolCancellationToken,
    run_cancel: Option<OpenAiCompletionsCancellationToken>,
) -> ToolDispatchResult {
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
            turns.push(tool_error_turn(
                tool_call,
                format!("unknown tool `{}`", tool_call.name),
            ));
            continue;
        };

        let args = match serde_json::from_str(&tool_call.arguments) {
            Ok(args) => args,
            Err(error) => {
                turns.push(tool_error_turn(
                    tool_call,
                    format!("tool call arguments are not valid JSON: {error}"),
                ));
                continue;
            }
        };

        let result = runtime.block_on(tool.execute(context, args, cancel.clone()));
        if cancel.is_cancelled() {
            return ToolDispatchResult::Cancelled;
        }

        match result {
            Ok(output) => turns.push(Turn::tool_result(&tool_call.id, output.content)),
            Err(error) => turns.push(tool_error_turn(tool_call, error.message())),
        }
    }

    ToolDispatchResult::Completed(turns)
}

fn tool_error_turn(tool_call: &ToolCall, message: impl Into<String>) -> Turn {
    Turn::tool_result(&tool_call.id, structured_tool_error(message))
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

    use serde_json::{Value, json};

    use super::*;
    use crate::sessions::ToolCall;
    use crate::tools::{
        NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolFuture, ToolOutput,
        ToolRegistry, read,
    };
    use crate::workspace::path::WorkspacePathPolicy;

    #[test]
    fn dispatches_single_tool_call_success_as_tool_turn() {
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).expect("echo should register");
        let tool_calls = vec![ToolCall {
            id: "call_echo_1".to_string(),
            name: "echo".to_string(),
            arguments: r#"{"text":"hello"}"#.to_string(),
        }];

        let result = dispatch_tool_calls(
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
                name: "echo".to_string(),
                arguments: r#"{"text":"first"}"#.to_string(),
            },
            ToolCall {
                id: "call_echo_2".to_string(),
                name: "echo".to_string(),
                arguments: r#"{"text":"second"}"#.to_string(),
            },
        ];

        let result = dispatch_tool_calls(
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
            name: "echo".to_string(),
            arguments: "not json".to_string(),
        }];

        let result = dispatch_tool_calls(
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
            name: "missing".to_string(),
            arguments: "{}".to_string(),
        }];

        let result = dispatch_tool_calls(
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
            name: "wait".to_string(),
            arguments: "{}".to_string(),
        }];
        let cancel = ToolCancellationToken::new();
        let cancel_from_thread = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel_from_thread.cancel();
        });

        let result = dispatch_tool_calls(
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
            name: "wait".to_string(),
            arguments: "{}".to_string(),
        }];
        let run_cancel = OpenAiCompletionsCancellationToken::new();
        let cancel_from_thread = run_cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            cancel_from_thread.cancel();
        });

        let result = dispatch_tool_calls(
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
            name: "read".to_string(),
            arguments: r#"{"path":"../secret.txt"}"#.to_string(),
        }];

        let result = dispatch_tool_calls(
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
                "properties": {
                    "text": { "type": "string" }
                },
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
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })
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
