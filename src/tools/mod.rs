//! Agent tools: a small fixed toolset the model can call, mirroring pi's
//! coding agent. Each tool resolves paths relative to a working directory and
//! caps its output so one call can't blow the model's context window.
//!
//! Safety posture (v1): tools run with the backend user's privileges. Path
//! tools refuse to escape the workspace, `bash` is time-bounded and exempt
//! (it runs with your shell's privileges), and every tool caps its output. The
//! backend is expected to stay bound to loopback.
//!
//! The [`Registry`] is the seam used by the agent: it advertises tool
//! definitions and executes Tool Calls. Model-visible Tools live in
//! `builtins/`; shared implementation helpers live in `support/`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::Value;

use crate::model::{ToolCall, ToolDef};

mod builtins;
mod support;

#[cfg(test)]
mod tests;

/// Cooperative cancellation shared with a running tool. A long `bash`/`grep`
/// checks this and aborts when it flips to `true`.
pub type CancelFlag = Arc<AtomicBool>;

/// A successful tool result: the text handed back to the model.
#[derive(Debug)]
pub struct ToolOutput {
    pub content: String,
}

impl ToolOutput {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }
}

/// A tool implementation failure. The registry turns this into an error tool
/// result fed back to the model (so it can recover), not a run failure.
#[derive(Debug)]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// A completed tool call as the agent sees it: text plus whether it should be
/// fed back to the model as an error result.
#[derive(Debug, PartialEq, Eq)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    fn ok(output: ToolOutput) -> Self {
        Self {
            content: output.content,
            is_error: false,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            content: message.into(),
            is_error: true,
        }
    }
}

/// One callable tool: a name + description + JSON-Schema parameters the model
/// sees, plus an executor bound to a working directory.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema object describing the tool's parameters.
    fn parameters(&self) -> Value;
    /// One-line description shown in the system prompt's "Available tools" list.
    /// Mirrors pi's per-tool `promptSnippet`; a tool that returns `None` is
    /// omitted from that list.
    fn prompt_snippet(&self) -> Option<&'static str> {
        None
    }
    /// Extra guideline bullets this tool contributes to the system prompt.
    /// Mirrors pi's per-tool `promptGuidelines`; empty by default.
    fn prompt_guidelines(&self) -> &'static [&'static str] {
        &[]
    }
    fn execute(
        &self,
        args: &Value,
        cwd: &Path,
        cancel: &CancelFlag,
    ) -> Result<ToolOutput, ToolError>;
}

/// The fixed set of tools available to a session, in a stable advertised order.
pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
}

impl Registry {
    /// The default coding toolset: read, bash, edit, write, grep, find, ls.
    pub fn coding() -> Self {
        Self {
            tools: builtins::coding_tools(),
        }
    }

    /// Tool definitions to advertise to the model.
    pub fn defs(&self) -> Vec<ToolDef> {
        self.tools
            .iter()
            .map(|tool| ToolDef {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: tool.parameters(),
            })
            .collect()
    }

    /// Tool names in advertised order, for the system prompt's tool list.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect()
    }

    /// One-line prompt snippets keyed by tool name (only tools that declare one).
    pub fn prompt_snippets(&self) -> HashMap<String, String> {
        self.tools
            .iter()
            .filter_map(|tool| {
                tool.prompt_snippet()
                    .map(|snippet| (tool.name().to_owned(), snippet.to_owned()))
            })
            .collect()
    }

    /// Guideline bullets contributed by the tools, in advertised order.
    pub fn prompt_guidelines(&self) -> Vec<String> {
        self.tools
            .iter()
            .flat_map(|tool| {
                tool.prompt_guidelines()
                    .iter()
                    .map(|guideline| (*guideline).to_owned())
            })
            .collect()
    }

    /// Execute one model-requested tool call.
    ///
    /// Tool failures are returned as error tool results, not as run failures,
    /// so the model can recover on the next turn.
    pub fn execute_call(&self, call: &ToolCall, cwd: &Path, cancel: &CancelFlag) -> ToolResult {
        let Some(tool) = self.tool(&call.name) else {
            return ToolResult::error(format!("unknown tool: {}", call.name));
        };
        let args = match parse_arguments(&call.arguments) {
            Ok(args) => args,
            Err(error) => return ToolResult::error(format!("invalid tool arguments: {error}")),
        };

        match tool.execute(&args, cwd, cancel) {
            Ok(output) => ToolResult::ok(output),
            Err(error) => ToolResult::error(error.message),
        }
    }

    fn tool(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.as_ref())
    }
}

fn parse_arguments(arguments: &str) -> Result<Value, serde_json::Error> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        Ok(Value::Object(Default::default()))
    } else {
        serde_json::from_str(trimmed)
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::coding()
    }
}

/// Read a required string argument.
fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new(format!("missing required string argument: {key}")))
}

/// Read an optional string argument.
fn arg_opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Read an optional unsigned-integer argument (accepts JSON numbers).
fn arg_opt_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

/// Read an optional boolean argument.
fn arg_opt_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}
