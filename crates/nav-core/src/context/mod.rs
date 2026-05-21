//! Context management: project context, skills, extensions, replay,
//! attachments, compaction, session history, and `/context` measurement.

use std::fmt::Write as _;
use std::path::Path;

pub use crate::agent_loop::UserAttachment;
pub use compaction::{
    AutoCompactDecision, CheckpointSlice, CompactionDetails, build_replacement_history,
    collect_recent_user_messages, is_summary_message, latest_checkpoint_slice, should_auto_compact,
    summary_message,
};
pub use extensions::{
    Extension, ExtensionCatalog, ExtensionScope, ExtensionTheme, PromptTemplate, ThemeColors,
    discover_extensions, load_prompt_template,
};
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
    SessionStore, SessionSummary, SessionTreeNode, TranscriptHit, export_events,
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

    let mut body = String::from("\n\nAvailable skills:\n");
    for skill in skills.iter() {
        // Absolute paths: the model loads these via the read_file tool,
        // which now accepts paths under any catalog skill_dir.
        let _ = writeln!(
            body,
            "- {name} [{scope}]: {description} (SKILL.md: {path}, skill_dir: {dir})",
            name = skill.name,
            scope = skill.scope.as_str(),
            description = skill.description,
            path = skill.skill_md_path.display(),
            dir = skill.skill_dir.display(),
        );
    }
    body.push_str(
        "When a user request matches a skill, read the listed SKILL.md \
         first to load its instructions before acting. Resolve any \
         relative resources mentioned in a SKILL.md against that skill's \
         skill_dir.",
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

pub mod compaction;

pub mod extensions;
pub mod project;
pub mod replay;
pub mod report;
pub mod session;
pub mod skills;
