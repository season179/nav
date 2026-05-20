//! Compatibility exports for the tool registry.
//!
//! New code should use [`crate::tool_registry`].

pub use crate::tool_registry::{
    BlockedTool, PermissionContext, PreflightOutcome, SPAWN_SUBAGENT_TOOL, ToolAccess, ToolOutcome,
    ToolResult, failed_mutation_summary, run_tool, unchecked_permission_context,
};

pub mod output_accumulator {
    //! Compatibility exports for bounded tool-output capture.

    pub use crate::tool_registry::output_accumulator::*;
}

pub mod preflight {
    //! Compatibility exports for tool preflight checks.

    pub use crate::guardrails::preflight::*;
}

pub mod truncate {
    //! Compatibility exports for shared tool-output truncation.

    pub use crate::tool_registry::truncate::*;
}
