//! Agent loop over one model and one tool registry.
//!
//! The agent owns the behavioral loop: call the model with the current Model
//! Context, execute requested tools, feed tool results back, and stop when the
//! model returns a plain assistant message. Callers provide an [`AgentRunSink`]
//! adapter to mirror those steps into session state, event streams, and
//! persistence.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::context::ModelContext;
use crate::model::{ChatMessage, ChatModel, ModelError, ToolCall};
use crate::tools::{CancelFlag, Registry};

/// Runs one coding-agent turn with a configured model, toolset, and workspace.
pub(crate) struct Agent {
    model: Arc<dyn ChatModel>,
    registry: Arc<Registry>,
    workspace: PathBuf,
}

impl Agent {
    pub(crate) fn new(model: Arc<dyn ChatModel>) -> Self {
        Self {
            model,
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
        mut context: ModelContext,
        cancel: &CancelFlag,
        sink: &mut S,
    ) -> Result<RunStop, AgentRunError<S::Error>>
    where
        S: AgentRunSink,
    {
        let tool_defs = self.registry.defs();

        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(RunStop::Cancelled);
            }

            let response = self
                .model
                .respond(&context, &tool_defs)
                .map_err(AgentRunError::Model)?;

            // A stop requested during the (blocking) model call takes effect now,
            // before the reply is emitted, so a cancelled run produces no final
            // assistant turn.
            if cancel.load(Ordering::Relaxed) {
                return Ok(RunStop::Cancelled);
            }

            if response.tool_calls.is_empty() {
                let reply = response.content.unwrap_or_default();
                sink.assistant_text(&reply).map_err(AgentRunError::Sink)?;
                return Ok(RunStop::Completed);
            }

            let content = response.content.unwrap_or_default();
            let calls = response.tool_calls;
            context.push(ChatMessage::assistant_tool_calls(&content, calls.clone()));
            sink.assistant_tool_calls(&content, &calls)
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
                let result = self.registry.execute_call(call, &self.workspace, cancel);
                context.push(ChatMessage::tool_result(
                    &call.id,
                    &result.content,
                    result.is_error,
                ));
                sink.tool_result(call, &result.content, result.is_error)
                    .map_err(AgentRunError::Sink)?;
            }
        }
    }
}

/// How an agent run ended once `run_turn` returned without error.
pub(crate) enum RunStop {
    /// The model produced a final assistant reply.
    Completed,
    /// The run was stopped by the shared cancel flag before completing.
    Cancelled,
}

/// Adapter notified by the agent loop as visible run steps happen.
pub(crate) trait AgentRunSink {
    type Error;

    fn assistant_text(&mut self, content: &str) -> Result<(), Self::Error>;

    fn assistant_tool_calls(
        &mut self,
        content: &str,
        calls: &[ToolCall],
    ) -> Result<(), Self::Error>;

    fn tool_started(&mut self, call: &ToolCall) -> Result<(), Self::Error>;

    fn tool_result(
        &mut self,
        call: &ToolCall,
        output: &str,
        is_error: bool,
    ) -> Result<(), Self::Error>;
}

/// Why an agent run stopped before producing a completed assistant response.
pub(crate) enum AgentRunError<E> {
    Model(ModelError),
    Sink(E),
}
