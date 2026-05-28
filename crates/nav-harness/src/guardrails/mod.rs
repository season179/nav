//! Hook-driven tool guardrails.
//!
//! Tool dispatch validates and parses the model's JSON arguments, builds a
//! [`ToolCallContext`], then runs these hooks before execution. The core stays
//! small: hooks can allow, deny, or request confirmation without teaching the
//! dispatcher a central tool-by-preset policy table.
//!
//! A [`BeforeToolCallDecision::RequestConfirmation`] fails closed today unless
//! the runner has an explicit scripted approval policy. APR-02a/APR-02b can
//! consume the same decision by emitting `tool.approval_requested`, pausing the
//! run, and resuming with an approval policy after `tool.approve`; no central
//! policy matrix is required. After hooks run on successful tool output before
//! it is persisted or returned to the model, so first-party hooks can redact or
//! normalize results at one boundary.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use nav_types::{RunId, ToolCallId};
use serde_json::Value;

use crate::tools::{RiskClass, ToolContext, ToolOutput, ToolPreset};

pub type GuardrailEngine = GuardrailRunner;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeforeToolCallDecision {
    Allow,
    Deny { reason: String },
    RequestConfirmation { reason: String, summary: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ConfirmationPolicy {
    #[default]
    FailClosed,
    ScriptedAllow,
}

pub trait ToolGuardrailHook: fmt::Debug + Send + Sync + 'static {
    fn name(&self) -> &str;

    fn order(&self) -> i32 {
        0
    }

    fn before_tool_call(
        &self,
        _context: &ToolCallContext,
    ) -> Result<BeforeToolCallDecision, GuardrailError> {
        Ok(BeforeToolCallDecision::Allow)
    }

    fn after_tool_call(
        &self,
        _context: &ToolCallContext,
        output: ToolOutput,
    ) -> Result<ToolOutput, GuardrailError> {
        Ok(output)
    }
}

#[derive(Clone, Default)]
pub struct GuardrailRunner {
    hooks: Vec<Arc<dyn ToolGuardrailHook>>,
    confirmation_policy: ConfirmationPolicy,
}

impl fmt::Debug for GuardrailRunner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuardrailRunner")
            .field(
                "hooks",
                &self
                    .hooks
                    .iter()
                    .map(|hook| hook.name())
                    .collect::<Vec<_>>(),
            )
            .field("confirmation_policy", &self.confirmation_policy)
            .finish()
    }
}

impl GuardrailRunner {
    pub fn with_confirmation_policy(mut self, confirmation_policy: ConfirmationPolicy) -> Self {
        self.confirmation_policy = confirmation_policy;
        self
    }

    pub fn register_hook(&mut self, hook: impl ToolGuardrailHook) -> Result<(), GuardrailError> {
        let name = hook.name().to_string();
        if name.is_empty() {
            return Err(GuardrailError::EmptyHookName);
        }
        if self.hooks.iter().any(|existing| existing.name() == name) {
            return Err(GuardrailError::DuplicateHook(name));
        }

        self.hooks.push(Arc::new(hook));
        self.hooks.sort_by(|left, right| {
            left.order()
                .cmp(&right.order())
                .then_with(|| left.name().cmp(right.name()))
        });
        Ok(())
    }

    pub fn before_tool_call(&self, context: &ToolCallContext) -> Result<(), GuardrailError> {
        for hook in &self.hooks {
            match hook
                .before_tool_call(context)
                .map_err(|error| error.with_hook_name(hook.name()))?
            {
                BeforeToolCallDecision::Allow => {}
                BeforeToolCallDecision::Deny { reason } => {
                    return Err(GuardrailError::denied(hook.name(), reason));
                }
                BeforeToolCallDecision::RequestConfirmation { reason, summary } => {
                    self.handle_confirmation_request(hook.name(), reason, summary)?
                }
            }
        }

        Ok(())
    }

    pub fn after_tool_call(
        &self,
        context: &ToolCallContext,
        output: ToolOutput,
    ) -> Result<ToolOutput, GuardrailError> {
        let mut output = output;
        for hook in &self.hooks {
            output = hook
                .after_tool_call(context, output)
                .map_err(|error| error.with_hook_name(hook.name()))?;
        }
        Ok(output)
    }

    fn handle_confirmation_request(
        &self,
        hook_name: &str,
        reason: String,
        summary: String,
    ) -> Result<(), GuardrailError> {
        match &self.confirmation_policy {
            ConfirmationPolicy::FailClosed => Err(GuardrailError::confirmation_required(
                hook_name, reason, summary,
            )),
            ConfirmationPolicy::ScriptedAllow => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BashConfirmationHook;

impl ToolGuardrailHook for BashConfirmationHook {
    fn name(&self) -> &str {
        "default-bash-confirmation"
    }

    fn before_tool_call(
        &self,
        context: &ToolCallContext,
    ) -> Result<BeforeToolCallDecision, GuardrailError> {
        if context.tool_name != "bash" {
            return Ok(BeforeToolCallDecision::Allow);
        }

        Ok(BeforeToolCallDecision::RequestConfirmation {
            reason: "bash command requires confirmation".to_string(),
            summary: context.arguments.summary.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WritePathPolicyHook;

impl ToolGuardrailHook for WritePathPolicyHook {
    fn name(&self) -> &str {
        "default-write-path-policy"
    }

    fn before_tool_call(
        &self,
        context: &ToolCallContext,
    ) -> Result<BeforeToolCallDecision, GuardrailError> {
        if context.tool_name != "write" {
            return Ok(BeforeToolCallDecision::Allow);
        }

        if let Some(error) = context.path_resolution_errors.first() {
            return Ok(BeforeToolCallDecision::Deny {
                reason: error.error.clone(),
            });
        }

        Ok(BeforeToolCallDecision::Allow)
    }
}

pub fn default_guardrails() -> GuardrailRunner {
    let mut guardrails = GuardrailRunner::default();
    guardrails
        .register_hook(BashConfirmationHook)
        .expect("built-in bash confirmation hook should register");
    guardrails
        .register_hook(WritePathPolicyHook)
        .expect("built-in write path policy hook should register");
    guardrails
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallContext {
    pub tool_name: String,
    pub arguments: ToolCallArguments,
    pub tool_mode: ToolMode,
    pub workspace: ToolWorkspaceContext,
    pub resolved_paths: Vec<ResolvedPathMetadata>,
    pub path_resolution_errors: Vec<PathResolutionErrorMetadata>,
    pub call_id: String,
    pub nav_tool_call_id: Option<ToolCallId>,
    pub run_id: RunId,
}

impl ToolCallContext {
    pub fn new(params: ToolCallContextParams<'_>) -> Self {
        let workspace = ToolWorkspaceContext::from_tool_context(params.tool_context);
        let path_metadata = collect_path_metadata(&params.parsed_arguments, params.tool_context);

        Self {
            tool_name: params.tool_name.to_string(),
            arguments: ToolCallArguments::new(params.raw_arguments, params.parsed_arguments),
            tool_mode: ToolMode {
                preset: params.preset,
                risk_class: params.risk_class,
            },
            workspace,
            resolved_paths: path_metadata.resolved_paths,
            path_resolution_errors: path_metadata.path_resolution_errors,
            call_id: params.call_id.to_string(),
            nav_tool_call_id: params.nav_tool_call_id,
            run_id: params.run_id,
        }
    }
}

pub struct ToolCallContextParams<'a> {
    pub tool_name: &'a str,
    pub raw_arguments: String,
    pub parsed_arguments: Value,
    pub preset: ToolPreset,
    pub risk_class: RiskClass,
    pub tool_context: &'a ToolContext,
    pub call_id: &'a str,
    pub nav_tool_call_id: Option<ToolCallId>,
    pub run_id: RunId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallArguments {
    pub raw: String,
    pub parsed: Value,
    pub summary: String,
}

impl ToolCallArguments {
    fn new(raw: String, parsed: Value) -> Self {
        let summary = summarize_arguments(&parsed);
        Self {
            raw,
            parsed,
            summary,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolMode {
    pub preset: ToolPreset,
    pub risk_class: RiskClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolWorkspaceContext {
    pub workspace_root: Option<PathBuf>,
    pub session_cwd: Option<PathBuf>,
}

impl ToolWorkspaceContext {
    fn from_tool_context(context: &ToolContext) -> Self {
        let Some(policy) = context.path_policy() else {
            return Self {
                workspace_root: None,
                session_cwd: None,
            };
        };

        Self {
            workspace_root: Some(policy.workspace_root().to_path_buf()),
            session_cwd: Some(policy.session_cwd().to_path_buf()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPathMetadata {
    pub input: String,
    pub resolved_path: PathBuf,
    pub exists: bool,
    pub workspace_root: PathBuf,
    pub session_cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathResolutionErrorMetadata {
    pub input: String,
    pub error: String,
    pub workspace_root: PathBuf,
    pub session_cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardrailError {
    EmptyHookName,
    DuplicateHook(String),
    HookFailed {
        hook_name: String,
        message: String,
    },
    Denied {
        hook_name: String,
        reason: String,
    },
    ConfirmationRequired {
        hook_name: String,
        reason: String,
        summary: String,
    },
}

impl GuardrailError {
    pub fn hook_failed(hook_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::HookFailed {
            hook_name: hook_name.into(),
            message: message.into(),
        }
    }

    pub fn denied(hook_name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Denied {
            hook_name: hook_name.into(),
            reason: reason.into(),
        }
    }

    pub fn confirmation_required(
        hook_name: impl Into<String>,
        reason: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self::ConfirmationRequired {
            hook_name: hook_name.into(),
            reason: reason.into(),
            summary: summary.into(),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::EmptyHookName => "guardrail hook name cannot be empty".to_string(),
            Self::DuplicateHook(name) => {
                format!("guardrail hook `{name}` is already registered")
            }
            Self::HookFailed { hook_name, message } => {
                format!("guardrail hook `{hook_name}` failed: {message}")
            }
            Self::Denied { hook_name, reason } => {
                format!("guardrail hook `{hook_name}` denied tool call: {reason}")
            }
            Self::ConfirmationRequired {
                hook_name,
                reason,
                summary,
            } => format!(
                "guardrail hook `{hook_name}` requested confirmation but no approval channel is available: {reason}; {summary}"
            ),
        }
    }

    fn with_hook_name(self, hook_name: &str) -> Self {
        match self {
            Self::HookFailed {
                hook_name: existing,
                message,
            } if existing.is_empty() => Self::HookFailed {
                hook_name: hook_name.to_string(),
                message,
            },
            other => other,
        }
    }
}

impl fmt::Display for GuardrailError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl Error for GuardrailError {}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PathMetadata {
    resolved_paths: Vec<ResolvedPathMetadata>,
    path_resolution_errors: Vec<PathResolutionErrorMetadata>,
}

fn collect_path_metadata(arguments: &Value, tool_context: &ToolContext) -> PathMetadata {
    let Some(policy) = tool_context.path_policy() else {
        return PathMetadata::default();
    };
    let Some(path) = arguments.get("path").and_then(Value::as_str) else {
        return PathMetadata::default();
    };

    match policy.resolve(path) {
        Ok(resolved) => PathMetadata {
            resolved_paths: vec![ResolvedPathMetadata {
                input: path.to_string(),
                resolved_path: resolved.path().to_path_buf(),
                exists: resolved.exists(),
                workspace_root: policy.workspace_root().to_path_buf(),
                session_cwd: policy.session_cwd().to_path_buf(),
            }],
            path_resolution_errors: Vec::new(),
        },
        Err(error) => PathMetadata {
            resolved_paths: Vec::new(),
            path_resolution_errors: vec![PathResolutionErrorMetadata {
                input: path.to_string(),
                error: error.to_string(),
                workspace_root: policy.workspace_root().to_path_buf(),
                session_cwd: policy.session_cwd().to_path_buf(),
            }],
        },
    }
}

fn summarize_arguments(arguments: &Value) -> String {
    const MAX_SUMMARY_CHARS: usize = 240;

    let mut summary = arguments.to_string();
    if summary.len() > MAX_SUMMARY_CHARS {
        let mut cutoff = MAX_SUMMARY_CHARS;
        while !summary.is_char_boundary(cutoff) {
            cutoff -= 1;
        }
        summary.truncate(cutoff);
        summary.push_str("...");
    }
    summary
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use nav_types::RunId;
    use serde_json::json;

    use super::{
        BeforeToolCallDecision, ConfirmationPolicy, GuardrailError, GuardrailRunner,
        ToolCallContext, ToolCallContextParams, ToolGuardrailHook, default_guardrails,
    };
    use crate::tools::{RiskClass, ToolContext, ToolPreset};
    use crate::workspace::path::WorkspacePathPolicy;

    #[test]
    fn before_tool_call_allows_when_no_hooks_are_registered() {
        let runner = GuardrailRunner::default();

        runner
            .before_tool_call(&tool_call_context(json!({"path": "Cargo.toml"})))
            .expect("empty guardrail runner should allow");
    }

    #[test]
    fn before_hooks_run_in_order_and_stop_on_deny() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut runner = GuardrailRunner::default();
        runner
            .register_hook(RecordingHook::allow("zeta", seen.clone()))
            .expect("zeta hook should register");
        runner
            .register_hook(RecordingHook::deny(
                "alpha",
                seen.clone(),
                "blocked by alpha",
            ))
            .expect("alpha hook should register");
        runner
            .register_hook(RecordingHook::allow("omega", seen.clone()))
            .expect("omega hook should register");

        let error = runner
            .before_tool_call(&tool_call_context(json!({"path": "Cargo.toml"})))
            .expect_err("deny should stop dispatch");

        assert_eq!(
            seen.lock().expect("seen should be available").as_slice(),
            ["alpha"]
        );
        assert_eq!(error, GuardrailError::denied("alpha", "blocked by alpha"));
    }

    #[test]
    fn before_hooks_use_order_then_name_for_deterministic_ordering() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut runner = GuardrailRunner::default();
        runner
            .register_hook(RecordingHook::allow_with_order("middle", seen.clone(), 0))
            .expect("middle hook should register");
        runner
            .register_hook(RecordingHook::allow_with_order("zeta", seen.clone(), -10))
            .expect("zeta hook should register");
        runner
            .register_hook(RecordingHook::allow_with_order("alpha", seen.clone(), -10))
            .expect("alpha hook should register");

        runner
            .before_tool_call(&tool_call_context(json!({"path": "Cargo.toml"})))
            .expect("all hooks should allow");

        assert_eq!(
            seen.lock().expect("seen should be available").as_slice(),
            ["alpha", "zeta", "middle"]
        );
    }

    #[test]
    fn confirmation_requests_fail_closed_without_approval_channel() {
        let mut runner = GuardrailRunner::default();
        runner
            .register_hook(RecordingHook::request_confirmation(
                "confirm-test",
                Arc::new(Mutex::new(Vec::new())),
                "bash is risky",
                "Run `cargo test`",
            ))
            .expect("confirmation hook should register");

        let error = runner
            .before_tool_call(&tool_call_context(json!({"cmd": "cargo test"})))
            .expect_err("confirmation should fail closed without an approval channel");

        assert_eq!(
            error,
            GuardrailError::confirmation_required(
                "confirm-test",
                "bash is risky",
                "Run `cargo test`",
            )
        );
    }

    #[test]
    fn confirmation_requests_can_use_explicit_scripted_approval() {
        let mut runner =
            GuardrailRunner::default().with_confirmation_policy(ConfirmationPolicy::ScriptedAllow);
        runner
            .register_hook(RecordingHook::request_confirmation(
                "confirm-test",
                Arc::new(Mutex::new(Vec::new())),
                "bash is risky",
                "Run `cargo test`",
            ))
            .expect("confirmation hook should register");

        runner
            .before_tool_call(&tool_call_context(json!({"cmd": "cargo test"})))
            .expect("scripted approval should allow confirmation requests");
    }

    #[test]
    fn default_guardrails_request_confirmation_for_bash_only() {
        let runner = default_guardrails();

        let error = runner
            .before_tool_call(&tool_call_context_for_tool(
                "bash",
                RiskClass::Exec,
                json!({"command": "cargo test"}),
            ))
            .expect_err("bash should request confirmation by default");

        assert_eq!(
            error,
            GuardrailError::confirmation_required(
                "default-bash-confirmation",
                "bash command requires confirmation",
                r#"{"command":"cargo test"}"#,
            )
        );
        runner
            .before_tool_call(&tool_call_context_for_tool(
                "read",
                RiskClass::Read,
                json!({"path": "Cargo.toml"}),
            ))
            .expect("read should not be blocked by the bash confirmation hook");
    }

    #[test]
    fn default_guardrails_deny_write_when_path_policy_rejects_path() {
        let workspace = TestWorkspace::new("default_write_path_policy");
        let tool_context = ToolContext::with_path_policy(workspace.policy());
        let context = ToolCallContext::new(ToolCallContextParams {
            tool_name: "write",
            raw_arguments: r#"{"path":"../secret.txt","content":"nope"}"#.to_string(),
            parsed_arguments: json!({
                "path": "../secret.txt",
                "content": "nope",
            }),
            preset: ToolPreset::Coding,
            risk_class: RiskClass::Mutate,
            tool_context: &tool_context,
            call_id: "call_write_1",
            nav_tool_call_id: None,
            run_id: RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001")
                .expect("run id should parse"),
        });

        let error = default_guardrails()
            .before_tool_call(&context)
            .expect_err("write outside workspace should be denied by a default hook");

        assert!(
            matches!(
                error,
                GuardrailError::Denied {
                    ref hook_name,
                    ref reason
                } if hook_name == "default-write-path-policy"
                    && reason.contains("escapes workspace")
            ),
            "unexpected guardrail error: {error:?}"
        );
    }

    #[test]
    fn hook_errors_include_the_hook_name() {
        let mut runner = GuardrailRunner::default();
        runner
            .register_hook(FailingHook)
            .expect("failing hook should register");

        let error = runner
            .before_tool_call(&tool_call_context(json!({})))
            .expect_err("hook error should stop dispatch");

        assert_eq!(error, GuardrailError::hook_failed("broken-test", "boom"));
        assert_eq!(error.message(), "guardrail hook `broken-test` failed: boom");
    }

    #[test]
    fn tool_call_context_reuses_workspace_path_resolution_metadata() {
        let workspace = TestWorkspace::new("guardrail_path_metadata");
        workspace.create_dir("src");
        workspace.create_file("src/lib.rs");
        let tool_context = ToolContext::with_path_policy(workspace.policy());

        let context = tool_call_context_with_tool_context(
            json!({"path": "src/lib.rs", "limit": 1}),
            &tool_context,
        );

        assert_eq!(context.arguments.raw, r#"{"limit":1,"path":"src/lib.rs"}"#);
        assert_eq!(context.arguments.summary, context.arguments.raw);
        assert_eq!(
            context.workspace.workspace_root.as_deref(),
            Some(workspace.root())
        );
        assert_eq!(
            context.workspace.session_cwd.as_deref(),
            Some(workspace.root())
        );
        assert_eq!(context.resolved_paths.len(), 1);
        assert_eq!(context.resolved_paths[0].input, "src/lib.rs");
        assert_eq!(
            context.resolved_paths[0].resolved_path,
            workspace.root().join("src/lib.rs")
        );
        assert!(context.resolved_paths[0].exists);
        assert!(context.path_resolution_errors.is_empty());
    }

    #[test]
    fn tool_call_context_records_workspace_path_resolution_errors() {
        let workspace = TestWorkspace::new("guardrail_path_error");
        let tool_context = ToolContext::with_path_policy(workspace.policy());

        let context =
            tool_call_context_with_tool_context(json!({"path": "../secret.txt"}), &tool_context);

        assert!(context.resolved_paths.is_empty());
        assert_eq!(context.path_resolution_errors.len(), 1);
        let error = &context.path_resolution_errors[0];
        assert_eq!(error.input, "../secret.txt");
        assert!(error.error.contains("escapes workspace"));
        assert_eq!(error.workspace_root.as_path(), workspace.root());
        assert_eq!(error.session_cwd.as_path(), workspace.root());
    }

    #[test]
    fn argument_summary_truncates_without_splitting_utf8() {
        let context = tool_call_context(json!({
            "text": "🙂".repeat(100),
        }));

        assert!(context.arguments.summary.ends_with("..."));
        assert!(context.arguments.summary.len() <= 243);
    }

    fn tool_call_context(arguments: serde_json::Value) -> ToolCallContext {
        let tool_context = ToolContext::default();
        tool_call_context_with_tool_context(arguments, &tool_context)
    }

    fn tool_call_context_for_tool(
        tool_name: &str,
        risk_class: RiskClass,
        arguments: serde_json::Value,
    ) -> ToolCallContext {
        let tool_context = ToolContext::default();
        ToolCallContext::new(ToolCallContextParams {
            tool_name,
            raw_arguments: arguments.to_string(),
            parsed_arguments: arguments,
            preset: ToolPreset::Coding,
            risk_class,
            tool_context: &tool_context,
            call_id: "call_test_1",
            nav_tool_call_id: None,
            run_id: RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001")
                .expect("run id should parse"),
        })
    }

    fn tool_call_context_with_tool_context(
        arguments: serde_json::Value,
        tool_context: &ToolContext,
    ) -> ToolCallContext {
        ToolCallContext::new(ToolCallContextParams {
            tool_name: "read",
            raw_arguments: arguments.to_string(),
            parsed_arguments: arguments,
            preset: ToolPreset::Coding,
            risk_class: RiskClass::Read,
            tool_context,
            call_id: "call_read_1",
            nav_tool_call_id: None,
            run_id: RunId::try_new("019f2f6f-f178-7a72-9f28-000000000001")
                .expect("run id should parse"),
        })
    }

    #[derive(Debug)]
    struct RecordingHook {
        name: &'static str,
        seen: Arc<Mutex<Vec<&'static str>>>,
        order: i32,
        decision: BeforeToolCallDecision,
    }

    impl RecordingHook {
        fn allow(name: &'static str, seen: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                name,
                seen,
                order: 0,
                decision: BeforeToolCallDecision::Allow,
            }
        }

        fn allow_with_order(
            name: &'static str,
            seen: Arc<Mutex<Vec<&'static str>>>,
            order: i32,
        ) -> Self {
            Self {
                name,
                seen,
                order,
                decision: BeforeToolCallDecision::Allow,
            }
        }

        fn request_confirmation(
            name: &'static str,
            seen: Arc<Mutex<Vec<&'static str>>>,
            reason: &'static str,
            summary: &'static str,
        ) -> Self {
            Self {
                name,
                seen,
                order: 0,
                decision: BeforeToolCallDecision::RequestConfirmation {
                    reason: reason.to_string(),
                    summary: summary.to_string(),
                },
            }
        }

        fn deny(
            name: &'static str,
            seen: Arc<Mutex<Vec<&'static str>>>,
            reason: &'static str,
        ) -> Self {
            Self {
                name,
                seen,
                order: 0,
                decision: BeforeToolCallDecision::Deny {
                    reason: reason.to_string(),
                },
            }
        }
    }

    impl ToolGuardrailHook for RecordingHook {
        fn name(&self) -> &str {
            self.name
        }

        fn order(&self) -> i32 {
            self.order
        }

        fn before_tool_call(
            &self,
            _context: &ToolCallContext,
        ) -> Result<BeforeToolCallDecision, GuardrailError> {
            self.seen
                .lock()
                .expect("seen should be available")
                .push(self.name);
            Ok(self.decision.clone())
        }
    }

    #[derive(Debug)]
    struct FailingHook;

    impl ToolGuardrailHook for FailingHook {
        fn name(&self) -> &str {
            "broken-test"
        }

        fn before_tool_call(
            &self,
            _context: &ToolCallContext,
        ) -> Result<BeforeToolCallDecision, GuardrailError> {
            Err(GuardrailError::hook_failed("", "boom"))
        }
    }

    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("nav-guardrails-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("workspace should be created");
            Self {
                root: fs::canonicalize(root).expect("workspace should canonicalize"),
            }
        }

        fn root(&self) -> &std::path::Path {
            &self.root
        }

        fn create_dir(&self, relative_path: &str) {
            fs::create_dir_all(self.root.join(relative_path)).expect("directory should be created");
        }

        fn create_file(&self, relative_path: &str) {
            fs::write(self.root.join(relative_path), "").expect("file should be written");
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
