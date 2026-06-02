//! Agent loop over one model and one tool registry.
//!
//! The agent owns the behavioral loop: call the model with the current Model
//! Context, execute requested tools, feed tool results back, and stop when the
//! model returns a plain assistant message. Callers provide an [`AgentRunSink`]
//! adapter to mirror those steps into session state, event streams, and
//! persistence.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::context::ModelContext;
use crate::model::{
    ChatMessage, ChatModel, ModelError, ProviderCallTrace, ResponseReasoningItem, ToolCall,
};
use crate::stacks::{ModelCallStack, ModelCallStackInput, build_model_call_stack};
use crate::system_prompt::{self, BuildSystemPromptOptions};
use crate::tokens::TokenUsage;
use crate::tools::{CancelFlag, Registry};

/// Runs one coding-agent turn with a configured model, toolset, and workspace.
pub(crate) struct Agent {
    model: Arc<RwLock<ActiveModel>>,
    registry: Arc<Registry>,
    workspace: PathBuf,
}

#[derive(Clone)]
struct ActiveModel {
    model: Arc<dyn ChatModel>,
    model_id: Option<String>,
}

impl Agent {
    pub(crate) fn new(model: Arc<dyn ChatModel>) -> Self {
        Self {
            model: Arc::new(RwLock::new(ActiveModel {
                model,
                model_id: None,
            })),
            registry: Arc::new(Registry::coding()),
            workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    pub(crate) fn with_registry(mut self, registry: Arc<Registry>) -> Self {
        self.registry = registry;
        self
    }

    pub(crate) fn with_workspace(mut self, workspace: PathBuf) -> Self {
        self.workspace = workspace;
        self
    }

    pub(crate) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn set_model(&self, model: Arc<dyn ChatModel>, model_id: Option<String>) {
        *self.model.write().unwrap() = ActiveModel { model, model_id };
    }

    pub(crate) fn set_model_id(&self, model_id: Option<String>) {
        self.model.write().unwrap().model_id = model_id;
    }

    fn active_model(&self) -> ActiveModel {
        self.model.read().unwrap().clone()
    }

    /// Build the system prompt for this run from the toolset, workspace, and any
    /// project context files. Rebuilt per run so the date and project context
    /// stay current. It rides ahead of the conversation and is captured verbatim
    /// in the request body of every model-call record.
    fn system_prompt(&self, workspace: &Path) -> String {
        let tool_snippets = self.registry.prompt_snippets();
        let prompt_guidelines = self.registry.prompt_guidelines();
        let selected_tools = self.registry.tool_names();
        let context_files = system_prompt::load_project_context_files(
            workspace,
            system_prompt::nav_agent_dir().as_deref(),
        );
        let date = system_prompt::current_date();
        system_prompt::build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: &selected_tools,
            tool_snippets: &tool_snippets,
            prompt_guidelines: &prompt_guidelines,
            cwd: workspace,
            context_files: &context_files,
            date: &date,
        })
    }

    /// Run the model/tool loop from the assembled context for one Run.
    ///
    /// The sink is notified as each visible step happens, before long-running
    /// tool calls start and immediately after they finish. A model error stops
    /// the loop; a tool error is returned to the model as an error tool result.
    ///
    /// The shared `cancel` flag lets a caller stop the run cooperatively: a
    /// long-running tool (e.g. `bash`) is interrupted in place, and the loop
    /// checks the flag before each model call so it never starts new work after
    /// a stop was requested. An in-flight model request still finishes before
    /// the loop can observe the stop, since the HTTP call itself is blocking.
    /// Returns how the run ended so the caller can emit the right terminal event.
    pub(crate) fn run_turn<S>(
        &self,
        run_id: &str,
        mut context: ModelContext,
        workspace: &Path,
        cancel: &CancelFlag,
        sink: &mut S,
    ) -> Result<RunStop, AgentRunError<S::Error>>
    where
        S: AgentRunSink,
    {
        let tool_defs = self.registry.defs();
        // Attach the system prompt once; it leads every model call this run and
        // is captured verbatim in each model-call record's request body.
        context = context.with_system_prompt(self.system_prompt(workspace));

        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(RunStop::Cancelled);
            }

            let active_model = self.active_model();
            let started_at_ms = now_ms();
            let started = Instant::now();
            let traced = match active_model.model.respond_with_trace(&context, &tool_defs) {
                Ok(traced) => traced,
                Err(error) => {
                    let duration_ms = elapsed_ms(started);
                    ModelCallCapture {
                        run_id,
                        started_at_ms,
                        duration_ms,
                    }
                    .record(
                        sink,
                        ModelCallStackOutcome {
                            status: "failed",
                            provider_trace: error.provider_trace.as_deref().cloned(),
                            token_usage: None,
                            error: Some(error.message.clone()),
                        },
                    )?;
                    return Err(AgentRunError::Model(error));
                }
            };
            let duration_ms = elapsed_ms(started);
            let stack_capture = ModelCallCapture {
                run_id,
                started_at_ms,
                duration_ms,
            };
            let response = traced.response;
            let provider_trace = traced.provider_trace;
            let usage = response.token_usage.clone().unwrap_or_else(|| {
                let input_estimate = active_model
                    .model
                    .estimate_context_tokens(&context, &tool_defs);
                let output_estimate = active_model.model.estimate_output_tokens(&response);
                crate::tokens::TokenUsage::estimated(input_estimate, output_estimate)
            });
            sink.token_usage(&usage).map_err(AgentRunError::Sink)?;

            // A stop requested during the (blocking) model call takes effect now,
            // before the reply is emitted, so a cancelled run produces no final
            // assistant turn.
            if cancel.load(Ordering::Relaxed) {
                stack_capture.record(
                    sink,
                    ModelCallStackOutcome {
                        status: "cancelled",
                        provider_trace,
                        token_usage: Some(usage),
                        error: Some(
                            "cancelled after model response before reply emission".to_owned(),
                        ),
                    },
                )?;
                return Ok(RunStop::Cancelled);
            }

            let reasoning_content = response.reasoning_content.clone();
            let response_reasoning_items = response.response_reasoning_items.clone();

            if response.tool_calls.is_empty() {
                let reply = response.content.unwrap_or_default();
                sink.assistant_text(
                    &reply,
                    reasoning_content.as_deref(),
                    &response_reasoning_items,
                    active_model.model_id.as_deref(),
                )
                .map_err(AgentRunError::Sink)?;
                let mut assistant_turn = ChatMessage::assistant(&reply);
                assistant_turn.reasoning_content = reasoning_content.clone();
                assistant_turn.response_reasoning_items = response_reasoning_items.clone();
                context.push(assistant_turn);
                // The reply ends the run unless a message arrived while it was
                // produced, in which case the run continues with that message
                // folded into the context (mid-run steering).
                match sink.next_input_or_finish().map_err(AgentRunError::Sink)? {
                    TurnContinuation::Continue(messages) => {
                        for message in &messages {
                            context.push(ChatMessage::user(message.as_str()));
                        }
                        stack_capture.record(
                            sink,
                            ModelCallStackOutcome {
                                status: "completed",
                                provider_trace,
                                token_usage: Some(usage),
                                error: None,
                            },
                        )?;
                        continue;
                    }
                    TurnContinuation::Done => {
                        stack_capture.record(
                            sink,
                            ModelCallStackOutcome {
                                status: "completed",
                                provider_trace,
                                token_usage: Some(usage),
                                error: None,
                            },
                        )?;
                        return Ok(RunStop::Completed);
                    }
                }
            }

            let content = response.content.unwrap_or_default();
            let calls = response.tool_calls;
            let mut assistant_turn = ChatMessage::assistant_tool_calls(&content, calls.clone());
            assistant_turn.reasoning_content = reasoning_content.clone();
            assistant_turn.response_reasoning_items = response_reasoning_items.clone();
            context.push(assistant_turn);
            sink.assistant_tool_calls(
                &content,
                reasoning_content.as_deref(),
                &response_reasoning_items,
                &calls,
                active_model.model_id.as_deref(),
            )
            .map_err(AgentRunError::Sink)?;

            for call in &calls {
                // A stop requested mid-batch must not let a later call in the
                // same turn start — writing tools (`write`/`edit`) don't poll the
                // flag, so only skipping them here keeps a cancel from mutating
                // the workspace. Each skipped call still gets a result so every
                // tool call keeps its matching result and the saved history stays
                // replayable.
                if cancel.load(Ordering::Relaxed) {
                    let note = "[cancelled before execution]";
                    context.push(ChatMessage::tool_result(&call.id, note, true));
                    sink.tool_result(call, note, true)
                        .map_err(AgentRunError::Sink)?;
                    continue;
                }
                sink.tool_started(call).map_err(AgentRunError::Sink)?;
                // Re-check at the dispatch boundary: `tool_started` took the
                // session lock and emitted an event, a window in which a stop
                // could land. Closing it here keeps a non-polling tool
                // (`write`/`edit`) from running after a stop was requested.
                if cancel.load(Ordering::Relaxed) {
                    let note = "[cancelled before execution]";
                    context.push(ChatMessage::tool_result(&call.id, note, true));
                    sink.tool_result(call, note, true)
                        .map_err(AgentRunError::Sink)?;
                    continue;
                }
                let result = self.registry.execute_call(call, workspace, cancel);
                context.push(ChatMessage::tool_result(
                    &call.id,
                    &result.content,
                    result.is_error,
                ));
                sink.tool_result(call, &result.content, result.is_error)
                    .map_err(AgentRunError::Sink)?;
            }

            // Fold any message sent while this tool batch ran into the context so
            // the next model call sees it. A stop takes precedence: a cancelled
            // run drops its still-queued steering rather than acting on it.
            if cancel.load(Ordering::Relaxed) {
                stack_capture.record(
                    sink,
                    ModelCallStackOutcome {
                        status: "cancelled",
                        provider_trace,
                        token_usage: Some(usage),
                        error: Some("cancelled after tool batch".to_owned()),
                    },
                )?;
                return Ok(RunStop::Cancelled);
            }
            let steering_messages = sink.take_steer().map_err(AgentRunError::Sink)?;
            for message in &steering_messages {
                context.push(ChatMessage::user(message.as_str()));
            }
            stack_capture.record(
                sink,
                ModelCallStackOutcome {
                    status: "completed",
                    provider_trace,
                    token_usage: Some(usage),
                    error: None,
                },
            )?;
        }
    }
}

struct ModelCallCapture<'a> {
    run_id: &'a str,
    started_at_ms: u64,
    duration_ms: f64,
}

struct ModelCallStackOutcome {
    status: &'static str,
    provider_trace: Option<ProviderCallTrace>,
    token_usage: Option<TokenUsage>,
    error: Option<String>,
}

impl ModelCallCapture<'_> {
    fn record<S>(
        &self,
        sink: &mut S,
        outcome: ModelCallStackOutcome,
    ) -> Result<(), AgentRunError<S::Error>>
    where
        S: AgentRunSink,
    {
        let stack = build_model_call_stack(ModelCallStackInput {
            id: Uuid::now_v7().to_string(),
            run_id: self.run_id.to_owned(),
            status: outcome.status.to_owned(),
            started_at_ms: self.started_at_ms,
            duration_ms: self.duration_ms,
            provider_trace: outcome.provider_trace,
            token_usage: outcome.token_usage,
            error: outcome.error,
        });
        sink.model_call_stack(stack).map_err(AgentRunError::Sink)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn elapsed_ms(started_at: Instant) -> f64 {
    let millis = started_at.elapsed().as_secs_f64() * 1000.0;
    (millis * 100.0).round() / 100.0
}

/// How an agent run ended once `run_turn` returned without error.
pub(crate) enum RunStop {
    /// The model produced a final assistant reply.
    Completed,
    /// The run was stopped by the shared cancel flag before completing.
    Cancelled,
}

/// What a finished model reply means for the run: end it, or keep going because
/// a message arrived mid-run and should be folded into the next model call.
pub(crate) enum TurnContinuation {
    /// Steering arrived during the run; fold these user messages into the live
    /// context and continue the same run.
    Continue(Vec<String>),
    /// No steering queued; the run has been finalized.
    Done,
}

/// Adapter notified by the agent loop as visible run steps happen.
pub(crate) trait AgentRunSink {
    type Error;

    fn assistant_text(
        &mut self,
        content: &str,
        reasoning_content: Option<&str>,
        response_reasoning_items: &[ResponseReasoningItem],
        model_id: Option<&str>,
    ) -> Result<(), Self::Error>;

    fn assistant_tool_calls(
        &mut self,
        content: &str,
        reasoning_content: Option<&str>,
        response_reasoning_items: &[ResponseReasoningItem],
        calls: &[ToolCall],
        model_id: Option<&str>,
    ) -> Result<(), Self::Error>;

    fn tool_started(&mut self, call: &ToolCall) -> Result<(), Self::Error>;

    fn tool_result(
        &mut self,
        call: &ToolCall,
        output: &str,
        is_error: bool,
    ) -> Result<(), Self::Error>;

    /// Drain any messages sent while the run has been executing, recording each
    /// as a user turn, and return their texts so the loop can fold them into the
    /// live context. Returns an empty vec when nothing is queued.
    fn take_steer(&mut self) -> Result<Vec<String>, Self::Error>;

    /// Called once the model returns a plain reply: fold in any queued steering
    /// and continue, or finalize the run when nothing is queued. The decision is
    /// made atomically with respect to a sender queuing more input.
    fn next_input_or_finish(&mut self) -> Result<TurnContinuation, Self::Error>;

    fn token_usage(&mut self, _usage: &crate::tokens::TokenUsage) -> Result<(), Self::Error> {
        Ok(())
    }

    fn model_call_stack(&mut self, _stack: ModelCallStack) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Why an agent run stopped before producing a completed assistant response.
pub(crate) enum AgentRunError<E> {
    Model(ModelError),
    Sink(E),
}
