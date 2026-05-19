use crate::cli::Args;
use crate::project::ProjectContext;
use crate::skills::Catalog;
use crate::tools::tool_definitions;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::Path;

/// Builds the JSON body for a `Responses` API create request.
///
/// Exposed at crate level so the agent loop and tests can share it with the
/// transport implementations without duplicating the schema.
pub(crate) fn response_body(
    args: &Args,
    cwd: &Path,
    input: &[Value],
    skills: &Catalog,
    context: Option<&ProjectContext>,
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
        "tools": tool_definitions(),
    })
}

fn build_instructions(cwd: &Path, skills: &Catalog, context: Option<&ProjectContext>) -> String {
    let mut out = format!(
        "You are a small coding agent running in {}. Use tools to inspect, edit, search, and verify code. Prefer small, explicit steps. Paths must be relative.",
        cwd.display()
    );
    if !skills.is_empty() {
        out.push_str("\n\nAvailable skills:\n");
        for skill in skills.iter() {
            // Absolute paths: the model loads these via the read_file tool,
            // which now accepts paths under any catalog skill_dir.
            let _ = writeln!(
                out,
                "- {name} [{scope}]: {description} (SKILL.md: {path}, skill_dir: {dir})",
                name = skill.name,
                scope = skill.scope.as_str(),
                description = skill.description,
                path = skill.skill_md_path.display(),
                dir = skill.skill_dir.display(),
            );
        }
        out.push_str(
            "When a user request matches a skill, read the listed SKILL.md \
             first to load its instructions before acting. Resolve any \
             relative resources mentioned in a SKILL.md against that skill's \
             skill_dir.",
        );
    }
    if let Some(context) = context
        && !context.context_files.is_empty()
    {
        out.push_str(
            "\n\nProject context follows. Treat each block as authoritative \
             guidance for this workspace.\n",
        );
        // user-scope first, project last — project gets the strongest recency
        // anchor at the end of the instructions.
        for file in &context.context_files {
            let _ = write!(
                out,
                "\n--- BEGIN {name} ({scope}) ---\n{body}\n--- END {name} ({scope}) ---\n",
                name = file.display_name,
                scope = file.scope.as_str(),
                body = file.bytes.trim_end_matches('\n'),
            );
        }
    }
    out
}
