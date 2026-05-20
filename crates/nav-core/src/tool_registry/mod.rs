//! Tool registry: model-visible tool definitions, access policy, dispatch, and
//! concrete tool adapters.

pub(crate) use definitions::tool_definitions;
pub use definitions::{SPAWN_SUBAGENT_TOOL, ToolAccess};
pub use dispatch::{
    BlockedTool, PermissionContext, PreflightOutcome, ToolOutcome, ToolResult,
    failed_mutation_summary, run_tool, unchecked_permission_context,
};

pub mod definitions;
mod dispatch;
mod fs;
pub mod output_accumulator;
mod patch;
pub mod preflight {
    //! Tool preflight checks before shell commands or protected reads execute.

    pub use crate::guardrails::preflight::*;
}
mod read_filter;
mod shell;
pub mod truncate;

// `bash` errors tend to appear at the tail (assert failures, panics, traceback
// footers), so it gets head+tail. `read_file` / `code_search` are head-only
// because the earliest matches/lines are the most useful.
pub(crate) const BASH_HEAD_LINES: usize = 200;
