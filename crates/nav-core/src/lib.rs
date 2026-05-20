//! Core library for `nav`.
//!
//! The crate is organized around the six visible parts of the agent harness:
//! tool registry, model, context management, guardrails, agent loop, and
//! verify. The older flat module names remain available as compatibility
//! exports while the implementation is moved behind those reader-facing parts.

pub mod agent_loop;
pub mod context;
pub mod guardrails;
pub mod model;
pub mod tool_registry;
pub mod verify;

// Compatibility modules. New code should prefer the six harness modules above
// when there is an obvious owner, but these paths stay public for downstream
// callers and for incremental refactoring.
pub mod agent;
pub mod auth;
pub mod cli;
pub mod context_report;
pub mod control;
pub mod doctor;
pub mod extensions;
pub mod git_checkpoint;
pub mod git_diff;
pub mod models;
pub mod mutation;
pub mod permissions;
pub mod project;
pub mod protocol;
pub mod responses;
pub mod sandbox;
pub mod session;
pub mod skills;
pub mod tools;

pub use agent_loop::{
    AgentEvent, CompactionTrigger, EventStream, ResponsesTransport, SessionBinding, TurnUsage,
    UserAttachment, run_agent, run_agent_with_control,
};
pub use agent_loop::{
    ControlPlane, PendingInput, PendingInputDraft, PendingInputMode, PendingSkill,
    PendingSteeringQueue, TurnControls,
};
pub use agent_loop::{
    HEADLESS_PROTOCOL_VERSION, JSONRPC_VERSION, METHOD_AGENT_EVENT, METHOD_APPROVAL_RESPOND,
    METHOD_SESSION_STARTED, agent_event_notification, session_started_notification,
};
pub use context::{Catalog, Skill, SkillScope, discover_skills};
pub use context::{
    ContextCategory, ContextItem, ContextMeasure, ContextReport, build_context_report,
    build_context_report_with_replay_cwd, rebuild_responses_input,
};
pub use context::{
    ContextFile, ContextScope, ProjectContext, Settings, WorkspaceStatus, load_project_context,
    shorten_home,
};
pub use context::{
    ExportFormat, PROVIDER_OPENAI_RESPONSES, ReportedCost, ResolveSessionError, SessionId,
    SessionStore, SessionSummary, SessionTreeNode, TranscriptHit, export_events,
    infer_export_format, layout_session_tree, resolved_db_path,
};
pub use context::{
    Extension, ExtensionCatalog, ExtensionScope, ExtensionTheme, PromptTemplate, ThemeColors,
    discover_extensions, load_prompt_template,
};
pub use git_checkpoint::{
    GitCheckpointAction, GitCheckpointOutcome, GitCheckpointStatus, GitStashEntry,
};
pub use model::{OpenAiTransport, RetryPolicy};
pub use permissions::{ApprovalReason, AskForApproval, BlockRule, ReviewDecision, SandboxPolicy};
pub use verify::{
    FileChangeKind, FileChangeSummary, FileDiffSummary, MutationResult, PatchApplyStatus, TurnDiff,
};
