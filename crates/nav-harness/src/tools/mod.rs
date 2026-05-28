//! Typed, permissioned, observable, and recoverable tool access.
//!
//! This module owns the registry/API shape plus built-in tool definitions.
//! Risky execution policy stays in guardrail hooks, outside individual tools.

pub mod bash;
pub mod file_queue;
pub mod ls;
pub mod read;
pub mod truncation;
pub mod write;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tokio::sync::Notify;

use crate::guardrails::GuardrailRunner;
use crate::workspace::path::WorkspacePathPolicy;

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;
pub type ToolResult = Result<ToolOutput, ToolError>;

pub trait NavTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    fn risk_class(&self) -> RiskClass;

    fn streams_output(&self) -> bool {
        false
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
        cancel: ToolCancellationToken,
    ) -> ToolFuture<'a>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiskClass {
    Read,
    Mutate,
    Exec,
    Search,
}

impl RiskClass {
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Mutate => "mutate",
            Self::Exec => "exec",
            Self::Search => "search",
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ToolContext {
    path_policy: Option<WorkspacePathPolicy>,
    guardrails: GuardrailRunner,
    output_sink: Option<ToolOutputSink>,
}

impl ToolContext {
    pub fn with_path_policy(path_policy: WorkspacePathPolicy) -> Self {
        Self {
            path_policy: Some(path_policy),
            guardrails: GuardrailRunner::default(),
            output_sink: None,
        }
    }

    pub fn with_guardrails(mut self, guardrails: GuardrailRunner) -> Self {
        self.guardrails = guardrails;
        self
    }

    pub fn with_output_sink(mut self, output_sink: ToolOutputSink) -> Self {
        self.output_sink = Some(output_sink);
        self
    }

    pub fn path_policy(&self) -> Option<&WorkspacePathPolicy> {
        self.path_policy.as_ref()
    }

    pub fn guardrails(&self) -> &GuardrailRunner {
        &self.guardrails
    }

    pub fn output_sink(&self) -> Option<&ToolOutputSink> {
        self.output_sink.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub file_changes: Vec<ToolFileChange>,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            file_changes: Vec::new(),
        }
    }

    pub fn with_file_changed(mut self, path: impl Into<String>) -> Self {
        self.file_changes.push(ToolFileChange { path: path.into() });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFileChange {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputDelta {
    pub stream: String,
    pub chunk: String,
}

#[derive(Clone)]
pub struct ToolOutputSink {
    queue: Arc<ToolOutputQueue>,
}

#[derive(Clone)]
pub struct ToolOutputReceiver {
    queue: Arc<ToolOutputQueue>,
}

struct ToolOutputQueue {
    state: std::sync::Mutex<ToolOutputQueueState>,
    notify: Notify,
    capacity: usize,
}

#[derive(Debug, Default)]
struct ToolOutputQueueState {
    deltas: VecDeque<ToolOutputDelta>,
    lossy: bool,
}

impl fmt::Debug for ToolOutputSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ToolOutputSink")
    }
}

impl fmt::Debug for ToolOutputReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ToolOutputReceiver")
    }
}

impl ToolOutputSink {
    pub fn bounded(capacity: usize) -> (Self, ToolOutputReceiver) {
        let queue = Arc::new(ToolOutputQueue {
            state: std::sync::Mutex::new(ToolOutputQueueState::default()),
            notify: Notify::new(),
            capacity,
        });

        (
            Self {
                queue: Arc::clone(&queue),
            },
            ToolOutputReceiver { queue },
        )
    }

    pub fn push(&self, delta: ToolOutputDelta) {
        let mut state = self.queue.state.lock().unwrap();
        if self.queue.capacity == 0 {
            state.lossy = true;
            return;
        }

        if state.deltas.len() == self.queue.capacity {
            state.deltas.pop_front();
            state.lossy = true;
        }

        state.deltas.push_back(delta);
        drop(state);
        self.queue.notify.notify_one();
    }

    pub fn push_chunk(&self, stream: impl Into<String>, chunk: impl Into<String>) {
        self.push(ToolOutputDelta {
            stream: stream.into(),
            chunk: chunk.into(),
        });
    }
}

impl ToolOutputReceiver {
    pub async fn recv(&self) -> ToolOutputDelta {
        loop {
            if let Some(delta) = self.try_pop() {
                return delta;
            }

            self.queue.notify.notified().await;
        }
    }

    pub fn drain(&self) -> Vec<ToolOutputDelta> {
        let mut state = self.queue.state.lock().unwrap();
        state.deltas.drain(..).collect()
    }

    pub fn is_lossy(&self) -> bool {
        self.queue.state.lock().unwrap().lossy
    }

    fn try_pop(&self) -> Option<ToolOutputDelta> {
        self.queue.state.lock().unwrap().deltas.pop_front()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ToolError {}

/// Parse an optional positive integer argument from a JSON tool parameter.
///
/// Shared by `read` and `ls` (and future tools) so the validation message
/// is consistent across all tools.
pub fn parse_optional_positive_usize(
    value: Option<&Value>,
    name: &str,
) -> Result<Option<usize>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(number) = value.as_u64() else {
        return Err(ToolError::new(format!(
            "argument `{name}` must be a positive integer"
        )));
    };
    let number = usize::try_from(number)
        .map_err(|_| ToolError::new(format!("argument `{name}` is too large")))?;
    if number == 0 {
        return Err(ToolError::new(format!(
            "argument `{name}` must be a positive integer"
        )));
    }
    Ok(Some(number))
}

#[derive(Debug)]
struct ToolCancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

#[derive(Debug, Clone)]
pub struct ToolCancellationToken {
    state: Arc<ToolCancellationState>,
}

impl Default for ToolCancellationToken {
    fn default() -> Self {
        Self {
            state: Arc::new(ToolCancellationState {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }
}

impl ToolCancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.state.cancelled.swap(true, Ordering::SeqCst) {
            self.state.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.state.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if self.is_cancelled() {
                return;
            }

            notified.await;
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolPreset {
    #[default]
    Coding,
    Readonly,
}

impl ToolPreset {
    const ALL: [Self; 2] = [Self::Coding, Self::Readonly];

    pub fn name(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Readonly => "readonly",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.name() == name)
    }
}

pub struct ToolRegistry {
    tools_by_name: BTreeMap<String, Arc<dyn NavTool>>,
    preset_tools: BTreeMap<ToolPreset, BTreeSet<String>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ToolRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolRegistry")
            .field("tools", &self.tools_by_name.keys().collect::<Vec<_>>())
            .field("preset_tools", &self.preset_tools)
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        let preset_tools = ToolPreset::ALL
            .into_iter()
            .map(|preset| (preset, BTreeSet::new()))
            .collect();

        Self {
            tools_by_name: BTreeMap::new(),
            preset_tools,
        }
    }

    pub fn register(&mut self, tool: impl NavTool + 'static) -> Result<(), ToolRegistryError> {
        let name = tool.name().to_string();

        if name.is_empty() {
            return Err(ToolRegistryError::EmptyName);
        }

        if self.tools_by_name.contains_key(&name) {
            return Err(ToolRegistryError::DuplicateTool(name));
        }

        self.tools_by_name.insert(name, Arc::new(tool));
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn NavTool>> {
        self.tools_by_name.get(name).cloned()
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools_by_name.keys().map(String::as_str).collect()
    }

    pub fn add_to_preset(
        &mut self,
        preset: ToolPreset,
        tool_name: &str,
    ) -> Result<(), ToolRegistryError> {
        if !self.tools_by_name.contains_key(tool_name) {
            return Err(ToolRegistryError::UnknownTool(tool_name.to_string()));
        }

        self.preset_tools
            .entry(preset)
            .or_default()
            .insert(tool_name.to_string());
        Ok(())
    }

    pub fn preset_names(&self) -> Vec<&'static str> {
        ToolPreset::ALL.into_iter().map(ToolPreset::name).collect()
    }

    pub fn preset_tool_names(&self, preset: ToolPreset) -> Vec<String> {
        self.preset_tools
            .get(&preset)
            .into_iter()
            .flat_map(|tools| tools.iter().cloned())
            .collect()
    }

    pub fn preset_tools(&self, preset: ToolPreset) -> Vec<Arc<dyn NavTool>> {
        self.preset_tools
            .get(&preset)
            .into_iter()
            .flat_map(|tools| tools.iter())
            .filter_map(|name| self.get(name))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRegistryError {
    EmptyName,
    DuplicateTool(String),
    UnknownTool(String),
}

impl fmt::Display for ToolRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => formatter.write_str("tool name cannot be empty"),
            Self::DuplicateTool(name) => write!(formatter, "tool `{name}` is already registered"),
            Self::UnknownTool(name) => write!(formatter, "tool `{name}` is not registered"),
        }
    }
}

impl Error for ToolRegistryError {}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::tools::{bash::BashTool, ls::LsTool, read::ReadTool};

    use super::{
        NavTool, RiskClass, ToolCancellationToken, ToolContext, ToolFuture, ToolOutput,
        ToolOutputDelta, ToolOutputSink, ToolPreset, ToolRegistry,
    };

    #[tokio::test]
    async fn registry_dispatches_echo_tool_by_name() {
        let mut registry = ToolRegistry::default();

        registry
            .register(EchoTool)
            .expect("echo should register successfully");

        let tool = registry.get("echo").expect("echo should be registered");
        let context = ToolContext::default();
        let cancel = ToolCancellationToken::new();
        let result = tool
            .execute(
                &context,
                json!({ "text": "hello from the registry" }),
                cancel,
            )
            .await
            .expect("echo should execute");

        assert_eq!(tool.description(), "Echoes the provided text.");
        assert_eq!(
            tool.parameters(),
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"],
                "additionalProperties": false
            })
        );
        assert_eq!(result.content, "hello from the registry");
        assert_eq!(tool.risk_class(), RiskClass::Read);
        assert_eq!(tool.risk_class().name(), "read");
    }

    #[test]
    fn default_presets_exist_and_do_not_expose_echo() {
        let mut registry = ToolRegistry::default();

        registry
            .register(EchoTool)
            .expect("echo should register successfully");

        assert_eq!(registry.preset_names(), vec!["coding", "readonly"]);
        assert_eq!(ToolPreset::default(), ToolPreset::Coding);
        assert_eq!(ToolPreset::from_name("coding"), Some(ToolPreset::Coding));
        assert_eq!(
            ToolPreset::from_name("readonly"),
            Some(ToolPreset::Readonly)
        );
        assert_eq!(ToolPreset::from_name("unknown"), None);
        assert_eq!(
            registry.preset_tool_names(ToolPreset::Coding),
            Vec::<String>::new()
        );
        assert_eq!(
            registry.preset_tool_names(ToolPreset::Readonly),
            Vec::<String>::new()
        );
    }

    #[test]
    fn registered_tools_can_be_added_to_presets() {
        let mut registry = ToolRegistry::default();

        registry
            .register(EchoTool)
            .expect("echo should register successfully");
        registry
            .add_to_preset(ToolPreset::Coding, "echo")
            .expect("registered tool should join preset");

        assert_eq!(
            registry.preset_tool_names(ToolPreset::Coding),
            vec!["echo".to_string()]
        );
    }

    #[test]
    fn tool_names_returns_registered_tools_in_sorted_order() {
        let mut registry = ToolRegistry::default();

        registry
            .register(NamedTool("zeta"))
            .expect("zeta should register");
        registry
            .register(NamedTool("alpha"))
            .expect("alpha should register");

        assert_eq!(registry.tool_names(), vec!["alpha", "zeta"]);
    }

    #[test]
    fn risk_class_names_match_the_tool_contract() {
        assert_eq!(RiskClass::Read.name(), "read");
        assert_eq!(RiskClass::Mutate.name(), "mutate");
        assert_eq!(RiskClass::Exec.name(), "exec");
        assert_eq!(RiskClass::Search.name(), "search");
    }

    #[test]
    fn only_bash_opts_into_live_output_streaming() {
        assert!(BashTool.streams_output());
        assert!(!ReadTool.streams_output());
        assert!(!LsTool.streams_output());
    }

    #[test]
    fn output_sink_drops_oldest_pending_delta_and_marks_lossy() {
        let (sink, receiver) = ToolOutputSink::bounded(2);

        sink.push_chunk("stdout", "first");
        sink.push_chunk("stdout", "second");
        sink.push_chunk("stdout", "third");

        assert_eq!(
            receiver.drain(),
            vec![
                ToolOutputDelta {
                    stream: "stdout".to_string(),
                    chunk: "second".to_string(),
                },
                ToolOutputDelta {
                    stream: "stdout".to_string(),
                    chunk: "third".to_string(),
                },
            ]
        );
        assert!(receiver.is_lossy());
    }

    #[test]
    fn registry_rejects_duplicate_and_unknown_membership() {
        let mut registry = ToolRegistry::default();

        registry
            .register(EchoTool)
            .expect("first echo should register");

        let duplicate = registry
            .register(EchoTool)
            .expect_err("duplicate echo should be rejected");
        assert_eq!(duplicate.to_string(), "tool `echo` is already registered");

        let unknown = registry
            .add_to_preset(ToolPreset::Coding, "missing")
            .expect_err("unknown tool should not join preset");
        assert_eq!(unknown.to_string(), "tool `missing` is not registered");
    }

    struct EchoTool;

    impl NavTool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Echoes the provided text."
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
            _cancel: super::ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                Ok(ToolOutput::text(
                    args["text"].as_str().unwrap_or_default().to_string(),
                ))
            })
        }
    }

    struct NamedTool(&'static str);

    impl NavTool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            "A named test tool."
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
            _cancel: super::ToolCancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move { Ok(ToolOutput::text("")) })
        }
    }
}
