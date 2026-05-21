//! Context management: project context, skills, extensions, replay,
//! attachments, compaction, session history, and `/context` measurement.

use std::fmt::Write as _;
use std::path::Path;

pub use crate::agent_loop::UserAttachment;
pub use ambient::DEFAULT_AMBIENT_CONTEXT_TOKEN_BUDGET;
pub(crate) use ambient::{build_ambient_context, push_ambient_context};
pub(crate) use attachments::build_user_content;
pub use compaction::{
    AutoCompactDecision, CheckpointSlice, CompactionDetails, build_replacement_history,
    collect_recent_user_messages, is_summary_message, latest_checkpoint_slice, should_auto_compact,
    summary_message,
};
pub use extensions::{
    Extension, ExtensionCatalog, ExtensionScope, ExtensionTheme, PromptTemplate, ThemeColors,
    discover_extensions, load_prompt_template,
};
pub use handoff::{HANDOFF_SLASH, HandoffBudget, HandoffDraft, build_handoff_draft};
pub use project::{
    ContextFile, ContextScope, ProjectContext, Settings, WorkspaceStatus, load_project_context,
    shorten_home,
};
pub use replay::rebuild_responses_input;
pub use report::{
    ContextCategory, ContextItem, ContextMeasure, ContextReport, build_context_report,
    build_context_report_with_replay_cwd,
};
pub use session::{
    ExportFormat, PROVIDER_OPENAI_RESPONSES, ReportedCost, ResolveSessionError, SessionId,
    SessionStore, SessionSummary, SessionTreeNode, ThreadReadOptions, TranscriptHit, export_events,
    infer_export_format, layout_session_tree, resolved_db_path,
};
pub use skills::{Catalog, Skill, SkillScope, discover_skills};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstructionSectionKind {
    Base,
    Skills,
    ProjectContextIntro,
    ProjectContextFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstructionSection {
    pub kind: InstructionSectionKind,
    pub label: String,
    pub body: String,
}

pub(crate) fn instruction_sections(
    cwd: &Path,
    skills: &Catalog,
    context: Option<&ProjectContext>,
) -> Vec<InstructionSection> {
    let mut sections = vec![base_instruction_section(cwd)];
    if let Some(section) = skill_instruction_section(skills) {
        sections.push(section);
    }
    sections.extend(project_context_sections(context));
    sections
}

fn base_instruction_section(cwd: &Path) -> InstructionSection {
    InstructionSection {
        kind: InstructionSectionKind::Base,
        label: "base instructions".to_string(),
        body: format!(
            "\
You are a small coding agent running in {}.

Guidelines:
- Use tools to inspect, edit, search, and verify code.
- Prefer small, explicit steps.
- Keep responses concise.
- Explain technical details in plain, layman's terms.
- Show file paths clearly when working with files.
- Paths must be relative.",
            cwd.display()
        ),
    }
}

fn skill_instruction_section(skills: &Catalog) -> Option<InstructionSection> {
    if skills.is_empty() {
        return None;
    }

    // Catalog entries are intentionally compact so the static prompt prefix
    // stays cacheable. Skill bodies are not preloaded; the model loads a
    // `SKILL.md` only when a request matches a skill. Each entry now ships
    // the actual `skill_md_path` because frontmatter `name` may differ from
    // the directory basename — without the path, the model would try to
    // `read_file` `.agents/skills/<name>/SKILL.md` and miss the discovered
    // file.
    let mut body = String::from("\n\nAvailable skills (load each on demand):\n");
    for skill in skills.iter() {
        let _ = writeln!(
            body,
            "- {name} [{scope}]: {description} (read: {path})",
            name = skill.name,
            scope = skill.scope.as_str(),
            description = skill.description,
            path = skill.skill_md_path.display(),
        );
    }

    body.push_str(
        "When a user request matches a skill, read its `SKILL.md` (the \
         `read:` path on the line above) first to load full instructions, \
         then act. Resolve any relative resources mentioned in a SKILL.md \
         against that skill's directory.",
    );

    Some(InstructionSection {
        kind: InstructionSectionKind::Skills,
        label: format!("{} skill(s)", skills.len()),
        body,
    })
}

fn project_context_sections(context: Option<&ProjectContext>) -> Vec<InstructionSection> {
    let Some(context) = context else {
        return Vec::new();
    };
    if context.context_files.is_empty() {
        return Vec::new();
    }

    let mut sections = vec![InstructionSection {
        kind: InstructionSectionKind::ProjectContextIntro,
        label: "project context wrapper".to_string(),
        body: "\n\nProject context follows. Treat each block as authoritative guidance for this workspace.\n"
            .to_string(),
    }];
    // User-scope first, project last: project gets the strongest recency
    // anchor at the end of the instructions.
    sections.extend(
        context
            .context_files
            .iter()
            .map(project_context_file_section),
    );
    sections
}

fn project_context_file_section(file: &ContextFile) -> InstructionSection {
    InstructionSection {
        kind: InstructionSectionKind::ProjectContextFile,
        label: format!("{} ({})", file.display_name, file.scope.as_str()),
        body: format!(
            "\n--- BEGIN {name} ({scope}) ---\n{body}\n--- END {name} ({scope}) ---\n",
            name = file.display_name,
            scope = file.scope.as_str(),
            body = file.bytes.trim_end_matches('\n'),
        ),
    }
}

pub(crate) fn build_instructions(
    cwd: &Path,
    skills: &Catalog,
    context: Option<&ProjectContext>,
) -> String {
    instruction_sections(cwd, skills, context)
        .into_iter()
        .map(|section| section.body)
        .collect()
}

pub mod ambient;
pub(crate) mod attachments;
pub mod compaction;

pub mod extensions;
pub mod handoff;
pub(crate) mod history;
pub mod project;
pub mod replay;
pub mod replay_policy;
pub mod report;
pub mod session;
pub mod skills;
