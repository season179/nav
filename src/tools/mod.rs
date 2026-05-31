//! Agent tools: a small fixed toolset the model can call, mirroring pi's
//! coding agent. Each tool resolves paths relative to a working directory and
//! caps its output so one call can't blow the model's context window.
//!
//! Safety posture (v1): tools run with the backend user's privileges. Path
//! tools refuse to escape the workspace, `bash` is time-bounded and exempt
//! (it runs with your shell's privileges), and every tool caps its output. The
//! backend is expected to stay bound to loopback.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::Value;

use crate::model::ToolDef;

mod bash;
mod edit;
mod find;
mod glob;
mod grep;
mod ls;
mod paths;
mod read;
mod truncate;
mod walk;
mod write;

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

/// A tool failure. The agent loop turns this into an error tool result fed back
/// to the model (so it can recover), not a run failure.
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

/// One callable tool: a name + description + JSON-Schema parameters the model
/// sees, plus an executor bound to a working directory.
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema object describing the tool's parameters.
    fn parameters(&self) -> Value;
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
            tools: vec![
                Box::new(read::ReadTool),
                Box::new(bash::BashTool),
                Box::new(edit::EditTool),
                Box::new(write::WriteTool),
                Box::new(grep::GrepTool),
                Box::new(find::FindTool),
                Box::new(ls::LsTool),
            ],
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

    /// Look up a tool by the name the model called.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.as_ref())
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
