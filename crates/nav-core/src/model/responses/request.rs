use crate::cli::Args;
use crate::context::build_instructions;
use crate::context::{Catalog, ProjectContext};
use crate::tool_registry::{SPAWN_SUBAGENT_TOOL, ToolAccess, tool_definitions};
use serde_json::{Value, json};
use std::path::Path;

/// Test convenience wrapper for the normal full-tool request shape.
#[cfg(test)]
pub(crate) fn response_body(
    args: &Args,
    cwd: &Path,
    input: &[Value],
    skills: &Catalog,
    context: Option<&ProjectContext>,
) -> Value {
    response_body_with_options(
        args,
        cwd,
        input,
        skills,
        context,
        ResponseBodyOptions::default(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResponseBodyOptions {
    pub tool_access: ToolAccess,
    pub include_subagents: bool,
}

impl Default for ResponseBodyOptions {
    fn default() -> Self {
        Self {
            tool_access: ToolAccess::Full,
            include_subagents: true,
        }
    }
}

impl ResponseBodyOptions {
    pub(crate) fn read_only() -> Self {
        Self {
            tool_access: ToolAccess::ReadOnly,
            include_subagents: false,
        }
    }

    pub(crate) fn allows_tool(self, name: &str) -> bool {
        self.tool_access.allows(name) && (name != SPAWN_SUBAGENT_TOOL || self.include_subagents)
    }
}

pub(crate) fn response_body_with_options(
    args: &Args,
    cwd: &Path,
    input: &[Value],
    skills: &Catalog,
    context: Option<&ProjectContext>,
    options: ResponseBodyOptions,
) -> Value {
    // tools are just JSON descriptions. The model decides whether to emit
    // a function_call item; Rust remains responsible for actually doing work.
    json!({
        "model": args.model,
        "instructions": build_instructions(cwd, skills, context),
        "input": input,
        // store=false keeps the demo honest: nav manages the transcript itself,
        // and no server-side stored conversation is needed for the agent loop.
        "store": false,
        // With store=false, reasoning items must carry encrypted_content so
        // tool-call turns can replay them without referring to server state.
        "include": ["reasoning.encrypted_content"],
        "tools": tool_definitions(options.tool_access, options.include_subagents),
    })
}
