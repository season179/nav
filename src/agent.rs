//! Agent loop over one model and one tool registry.
//!
//! The agent owns the behavioral loop: call the model with the current history,
//! execute requested tools, feed tool results back, and stop when the model
//! returns a plain assistant message. Callers provide an [`AgentRunSink`] adapter
//! to mirror those steps into session state, event streams, and persistence.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::Value;

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

    /// Run the model/tool loop from a starting history.
    ///
    /// The sink is notified as each visible step happens, before long-running
    /// tool calls start and immediately after they finish. A model error stops
    /// the loop; a tool error is returned to the model as an error tool result.
    pub(crate) fn run_turn<S>(
        &self,
        mut history: Vec<ChatMessage>,
        sink: &mut S,
    ) -> Result<(), AgentRunError<S::Error>>
    where
        S: AgentRunSink,
    {
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let tool_defs = self.registry.defs();

        loop {
            let response = self
                .model
                .respond(&history, &tool_defs)
                .map_err(AgentRunError::Model)?;

            if response.tool_calls.is_empty() {
                let reply = response.content.unwrap_or_default();
                sink.assistant_text(&reply).map_err(AgentRunError::Sink)?;
                return Ok(());
            }

            let content = response.content.unwrap_or_default();
            let calls = response.tool_calls;
            history.push(ChatMessage::assistant_tool_calls(&content, calls.clone()));
            sink.assistant_tool_calls(&content, &calls)
                .map_err(AgentRunError::Sink)?;

            for call in &calls {
                sink.tool_started(call).map_err(AgentRunError::Sink)?;
                let (output, is_error) = self.run_tool(call, &cancel);
                history.push(ChatMessage::tool_result(&call.id, &output, is_error));
                sink.tool_result(call, &output, is_error)
                    .map_err(AgentRunError::Sink)?;
            }
        }
    }

    /// Execute one tool call, returning the text the next model call should see
    /// and whether the result represents a tool failure.
    fn run_tool(&self, call: &ToolCall, cancel: &CancelFlag) -> (String, bool) {
        let Some(tool) = self.registry.get(&call.name) else {
            return (format!("unknown tool: {}", call.name), true);
        };
        let trimmed = call.arguments.trim();
        let args: Value = if trimmed.is_empty() {
            Value::Object(Default::default())
        } else {
            match serde_json::from_str(trimmed) {
                Ok(args) => args,
                Err(error) => return (format!("invalid tool arguments: {error}"), true),
            }
        };
        match tool.execute(&args, &self.workspace, cancel) {
            Ok(output) => (output.content, false),
            Err(error) => (error.message, true),
        }
    }
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
