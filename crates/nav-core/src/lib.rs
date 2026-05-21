//! Core library for `nav`.
//!
//! The crate is organized around the six visible parts of the agent harness:
//! tool registry, model, context management, guardrails, agent loop, and
//! verify.

pub mod agent_loop;
pub mod cli;
pub mod context;
pub mod git_checkpoint;
pub mod guardrails;
pub mod model;
pub mod tool_registry;
pub mod verify;

mod permissions;
mod sandbox;

// Crate-root re-exports keep the public API pleasant: callers can import
// `nav_core::run_agent` instead of reaching through `nav_core::agent_loop`.
//
// Main entry point for driving one user turn through the agent loop.
pub use agent_loop::{AgentTurnRequest, SessionBinding, run_agent};

// Observable event/data types produced or consumed by the loop.
pub use agent_loop::{AgentEvent, CompactionTrigger, TurnUsage, UserAttachment};

// Model transport abstractions used by the loop.
pub use agent_loop::{EventStream, ResponsesTransport};

// Interactive control surface for queued follow-ups, steering, and turn state.
pub use agent_loop::{
    ControlPlane, PendingInput, PendingInputDraft, PendingInputMode, PendingSkill,
    PendingSteeringQueue, TurnControls,
};

// Headless JSON-RPC protocol constants and notification builders.
pub use agent_loop::{
    HEADLESS_PROTOCOL_VERSION, JSONRPC_VERSION, METHOD_AGENT_EVENT, METHOD_APPROVAL_RESPOND,
    METHOD_SESSION_STARTED, agent_event_notification, session_started_notification,
};

// Skill catalog discovery and the model-visible skill records it returns.
pub use context::{Catalog, Skill, SkillScope, discover_skills};

// Context measurement/reporting plus replay helpers for resumed sessions.
pub use context::{
    ContextCategory, ContextItem, ContextMeasure, ContextReport, build_context_report,
    build_context_report_with_replay_cwd, rebuild_responses_input,
};

// Focused prompt draft extraction for `/handoff <goal>`.
pub use context::{HANDOFF_SLASH, HandoffBudget, HandoffDraft, build_handoff_draft};

// Project context and settings loaded from the launch workspace.
pub use context::{
    ContextFile, ContextScope, ProjectContext, Settings, WorkspaceStatus, load_project_context,
    shorten_home,
};

// Session persistence, transcript search/export, and session tree layout.
pub use context::{
    ExportFormat, PROVIDER_OPENAI_RESPONSES, ReportedCost, ResolveSessionError, SessionId,
    SessionStore, SessionSummary, SessionTreeNode, ThreadReadOptions, TranscriptHit, export_events,
    infer_export_format, layout_session_tree, resolved_db_path,
};

// Extension discovery, prompt templates, and theme metadata.
pub use context::{
    Extension, ExtensionCatalog, ExtensionScope, ExtensionTheme, PromptTemplate, ThemeColors,
    discover_extensions, load_prompt_template,
};

// Git checkpoint/stash result types surfaced as durable agent events.
pub use git_checkpoint::{
    GitCheckpointAction, GitCheckpointOutcome, GitCheckpointStatus, GitStashEntry,
};

// Guardrail policy and approval-decision types shared by tools and frontends.
pub use guardrails::{ApprovalReason, AskForApproval, BlockRule, ReviewDecision, SandboxPolicy};

// Concrete OpenAI Responses transport and retry configuration.
pub use model::{OpenAiTransport, RetryPolicy};

// Verification summaries for file mutations and turn-level diffs.
pub use verify::{
    FileChangeKind, FileChangeSummary, FileDiffSummary, MutationResult, PatchApplyStatus, TurnDiff,
};
